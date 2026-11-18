//! GPUI views for the Threadlane mobile client.
//!
//! A connect (pairing entry) screen in front of a tabbed shell: a chats
//! tab holding the session list and the live transcript with composer, a
//! Git tab for the selected project's repository, and a slide-over
//! sidebar for project picking and connection controls. All daemon
//! traffic flows through [`crate::client::MobileDaemon`]; the view pumps
//! its event stream on the GPUI executor and keeps a flat projection of
//! the wire types.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use gpui::{prelude::*, *};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tag::Tag;
use gpui_kit::component::StyledExt;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState, Textarea},
    marker::{Marker, MarkerContent, MarkerLoadingStyle},
    ActiveTheme, Disableable, Icon, Placement, Sizable, WindowExt,
};
use gpui_kit_assets::__private as kit_icons;
use threadlane_protocol::automation::{
    AutomationCommand, AutomationResponse, Definition, Run, RunStatus, Schedule,
};
use threadlane_protocol::daemon::{CommandResponse, ComposerModel};
use threadlane_protocol::repo::{
    CheckoutMode, DiffOptions, GitBranchInfo, GitCommitInfo, GitFile,
    GitHubIssueDetail, GitHubIssueListState, GitHubIssueSummary, GitHubOperation,
    GitHubPrInfo, GitHubPrListState, GitHubPullRequestSummary, GitHubResponse,
    GitOperation, GitResponse, GitStashInfo, GitStatus, PrCheckStatus,
};
use threadlane_protocol::{OrchestratorMode, ReasoningEffort};

use crate::client::{MobileDaemon, MobileEvent};
use crate::preferences;
use crate::saved_devices::{SavedDevice, SavedDeviceInfo, SavedDeviceList};
use threadlane_client::ClientState;
use threadlane_protocol::daemon::{
    ChatMessageInfo, GitHubIssueRef, MessageRole, PermissionDecision, SessionCommand, SessionEvent,
    SessionHealth, SessionInfo,
};
use threadlane_protocol::interaction::{
    PermissionRequest, QuestionAnswer, QuestionItemAnswer, QuestionRequest,
};

/// Deep links delivered by `set_deep_link_handler` on the UIKit thread.
/// The view drains this on a short timer — the handler runs outside GPUI
/// contexts so it can only stash the URL.
static PENDING_LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Stash a `threadlane://` deep link for the view to consume.
pub fn push_deeplink(url: String) {
    PENDING_LINKS.lock().unwrap().push(url);
}

fn take_pending_links() -> Vec<String> {
    std::mem::take(&mut *PENDING_LINKS.lock().unwrap())
}

fn composer_command_session(command: &SessionCommand) -> Option<&str> {
    match command {
        SessionCommand::GetComposerOptions { session_id, .. } => session_id.as_deref(),
        SessionCommand::SetModel { session_id, .. }
        | SessionCommand::SetReasoningEffort { session_id, .. }
        | SessionCommand::SetOrchestratorMode { session_id, .. } => Some(session_id),
        _ => None,
    }
}

const PREF_HOST: &str = "threadlane.pair.host";
const PREF_PORT: &str = "threadlane.pair.port";
const PREF_TOKEN: &str = "threadlane.pair.token";
const PREF_DEVICES: &str = "threadlane.pair.devices.v1";

#[cfg(target_os = "ios")]
fn local_device_name() -> String {
    use std::ffi::CStr;

    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    unsafe {
        let device: *mut AnyObject = msg_send![class!(UIDevice), currentDevice];
        if device.is_null() {
            return "iPhone".to_string();
        }
        let name: *mut AnyObject = msg_send![device, name];
        if name.is_null() {
            return "iPhone".to_string();
        }
        let utf8: *const std::ffi::c_char = msg_send![name, UTF8String];
        if utf8.is_null() {
            return "iPhone".to_string();
        }
        let value = CStr::from_ptr(utf8).to_string_lossy().trim().to_string();
        if value.is_empty() {
            "iPhone".to_string()
        } else {
            value
        }
    }
}

#[cfg(not(target_os = "ios"))]
fn local_device_name() -> String {
    "Mobile device".to_string()
}

fn restore_saved_devices() -> Result<SavedDeviceList, String> {
    if let Some(value) = preferences::get_string(PREF_DEVICES) {
        return SavedDeviceList::from_json(&value);
    }
    let legacy = match (
        preferences::get_string(PREF_HOST),
        preferences::get_string(PREF_PORT),
        preferences::get_string(PREF_TOKEN),
    ) {
        (Some(host), Some(port), Some(token)) => {
            Some(SavedDeviceList::from_legacy(host, port, token)?)
        }
        _ => None,
    };
    let devices = legacy.unwrap_or_default();
    if !devices.rows().is_empty() {
        persist_saved_devices(&devices)?;
        clear_legacy_pairing();
    }
    Ok(devices)
}

fn persist_saved_devices(devices: &SavedDeviceList) -> Result<(), String> {
    preferences::set_string(PREF_DEVICES, &devices.to_json()?);
    Ok(())
}

fn clear_legacy_pairing() {
    preferences::remove(PREF_HOST);
    preferences::remove(PREF_PORT);
    preferences::remove(PREF_TOKEN);
}

static FOREGROUND_RECONNECT_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn request_foreground_reconnect() {
    FOREGROUND_RECONNECT_REQUESTED.store(true, Ordering::SeqCst);
}

#[derive(Clone)]
struct ConnectionSnapshot {
    device_id: Option<String>,
    name: Option<String>,
    host: String,
    port: u16,
    token: String,
}

/// Kit icons on iOS: there is no installable `AssetSource`, so `Icon::path`
/// cannot resolve — embed the SVG bytes straight from the generated
/// `embedded` constants instead.
fn icon(bytes: &'static [u8]) -> Icon {
    Icon::default().data(bytes)
}

/// Selection/custom-answer map key: one entry per request + question item.
fn question_key(request_id: &str, item_id: &str) -> String {
    format!("{request_id}\0{item_id}")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckState {
    Passing,
    Pending,
    Failing,
}

/// `gh` reports check statuses and conclusions in uppercase; this mirrors
/// the classification in `crates/threadlane-git/src/github.rs`.
fn check_state(check: &PrCheckStatus) -> CheckState {
    let conclusion = check.conclusion.as_deref().unwrap_or("").to_uppercase();
    if matches!(
        conclusion.as_str(),
        "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR"
    ) {
        CheckState::Failing
    } else if matches!(
        check.status.to_uppercase().as_str(),
        "IN_PROGRESS" | "QUEUED" | "PENDING" | "EXPECTED"
    ) || check.conclusion.is_none()
    {
        CheckState::Pending
    } else if matches!(conclusion.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED") {
        CheckState::Passing
    } else {
        CheckState::Pending
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Connect,
    Main,
}

/// A destination in the main screen's bottom tab bar. Tabs only appear
/// once their surface works — the bar grows as panels land.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Chats,
    Git,
    Issues,
    Pulls,
    Automations,
}

/// Schedule presets the automation form offers — the full calendar
/// editor is desktop-only for now. `Existing` preserves the loaded
/// definition's own schedule so editing an automation made on desktop
/// can't collapse its calendar to a preset.
#[derive(Clone, Copy, PartialEq)]
enum AutoSchedule {
    Existing,
    Manual,
    Interval,
    DailyUtc,
}

impl AutoSchedule {
    fn label(self) -> &'static str {
        match self {
            Self::Existing => "Keep existing schedule",
            Self::Manual => "Manual",
            Self::Interval => "Every N minutes",
            Self::DailyUtc => "Daily 09:00 UTC",
        }
    }
}

/// The session whose transcript the view is watching.
struct ActiveSession {
    id: String,
    title: String,
    /// `SessionInfo::runtime_work_dir` — the execution dir `SubmitPrompt` wants.
    work_dir: std::path::PathBuf,
    /// Needed by `DeleteSession` to archive the transcript.
    session_file: std::path::PathBuf,
    /// Queued question requests; the front entry is rendered.
    /// Toggled options per question item id, for the front request.
    answers: HashMap<String, Vec<String>>,
    transcript: threadlane_ui_kit::transcript::TranscriptState,
    confirm_delete: bool,
    /// Repo context mirrored from `SessionInfo` for the header meta row.
    git_branch: Option<String>,
    is_worktree: bool,
    /// Linked issue rendered as a chip that opens the Issues panel.
    github_issue: Option<GitHubIssueRef>,
}

impl ActiveSession {
    fn new(info: &SessionInfo, window: &Window) -> Self {
        Self {
            id: info.id.clone(),
            title: if info.title.trim().is_empty() {
                "Untitled session".to_string()
            } else {
                info.title.clone()
            },
            work_dir: info.runtime_work_dir.clone(),
            session_file: info.session_file.clone(),
            answers: HashMap::new(),
            transcript: threadlane_ui_kit::transcript::TranscriptState::new(window),
            confirm_delete: false,
            git_branch: info.git_branch.clone(),
            is_worktree: info.is_worktree,
            github_issue: info.github_issue.clone(),
        }
    }
}

/// Root view. One `MobileDaemon` drives all traffic; `screen` picks the
/// layout and `projects`/`active` hold the rendered projection.
pub struct MobileApp {
    screen: Screen,
    host: Entity<InputState>,
    port: Entity<InputState>,
    token: Entity<InputState>,
    search: Entity<InputState>,
    composer: Entity<gpui_kit::component::input::TextareaState>,
    /// One custom-answer input per `allow_custom` question item, keyed by
    /// `question_key(request.id, item.id)` and created lazily on render.
    /// The keyboard subscription rides along so it drops with the input.
    question_inputs: HashMap<String, (Entity<InputState>, Subscription)>,
    connect_error: Option<String>,
    saved_device_list: SavedDeviceList,
    remove_device_confirmation: Option<String>,
    selected_device_id: Option<String>,
    active_device_id: Option<String>,
    connection_snapshot: Option<ConnectionSnapshot>,
    connection_generation: u64,
    removed_device_ids: HashSet<String>,
    sending: bool,
    pending_composer: HashMap<String, usize>,
    uncertain_prompts: HashSet<String>,
    daemon: Option<MobileDaemon>,
    /// Human-readable link state shown in the sessions header.
    link_state: String,
    /// Deep links already consumed, so reconnect flows don't re-apply them.
    seen_links: Vec<String>,
    client: ClientState,
    markdown_states:
        HashMap<(SharedString, String), threadlane_ui_kit::markdown::MarkdownRenderState>,
    active: Option<ActiveSession>,
    sessions_list: ListState,
    session_rows: Vec<SessionInfo>,
    project_git: HashMap<std::path::PathBuf, GitStatus>,
    /// Which tab the main screen shows.
    tab: Tab,
    /// Slide-over drawer with the project list and connection controls.
    sidebar_open: bool,
    /// Commit-message composer on the Git tab.
    git_message: Entity<gpui_kit::component::input::TextareaState>,
    /// New-branch name input on the Git tab.
    git_branch: Entity<InputState>,
    /// Open diff overlay: `(title, unified diff text)`.
    git_diff: Option<(String, String)>,
    /// Diff request in flight — the next `GitResponse::Text` fills `git_diff`.
    git_diff_pending: Option<String>,
    /// Expanded commit/stash objects on the Git tab, keyed
    /// `<work_dir>|commit-<sha>` or `<work_dir>|stash-<index>` — index
    /// keys repeat across projects.
    git_expanded: HashSet<String>,
    /// Files loaded for an expanded object, same key space; presence of
    /// the key distinguishes loaded-empty from still-loading.
    git_object_files: HashMap<String, Vec<GitFile>>,
    /// Issue list for the selected project; `None` until the first
    /// `ListIssues` reply lands.
    issues: Option<Vec<GitHubIssueSummary>>,
    issue_filter: GitHubIssueListState,
    issue_search: Entity<InputState>,
    /// Open issue detail overlay (body + comments).
    issue_detail: Option<GitHubIssueDetail>,
    /// `InspectIssue` request in flight.
    issue_detail_pending: Option<u64>,
    /// Confirm strip inside the detail overlay before `DeleteIssue`.
    issue_confirm_delete: bool,
    issue_comment: Entity<gpui_kit::component::input::TextareaState>,
    /// New-issue form state.
    new_issue_open: bool,
    issue_title: Entity<InputState>,
    issue_body: Entity<gpui_kit::component::input::TextareaState>,
    /// Pull-request list + detail for the selected project.
    prs: Option<Vec<GitHubPullRequestSummary>>,
    pr_filter: GitHubPrListState,
    pr_search: Entity<InputState>,
    pr_detail: Option<GitHubPrInfo>,
    pr_detail_pending: Option<u64>,
    pr_comment: Entity<gpui_kit::component::input::TextareaState>,
    /// Automation editor overlay: `None` when closed, `Some(definition)`
    /// carries the definition being edited (or the draft skeleton for a
    /// new one — `revision == 0` marks it new).
    auto_editing: Option<Definition>,
    auto_name: Entity<InputState>,
    auto_model: Entity<InputState>,
    auto_effort: Entity<InputState>,
    auto_minutes: Entity<InputState>,
    auto_prompt: Entity<gpui_kit::component::input::TextareaState>,
    auto_schedule: AutoSchedule,
    auto_worktree: bool,
    auto_enabled: bool,
    /// Definition id awaiting its delete-confirm strip.
    auto_delete_confirm: Option<String>,
    /// A queued-message cancel awaiting its reply or the journaled
    /// `QueuedEntryCancelled`: the echo row stays until the daemon
    /// confirms the entry left the queue. `(session_id, entry_id)`.
    pending_queued_cancel: Option<(String, String)>,
    /// Run id awaiting its delete-confirm strip.
    auto_run_delete_confirm: Option<String>,
    /// Comment text per detail target, keyed
    /// `<work_dir>|issue-<number>` / `<work_dir>|pr-<number>` — stashed
    /// on close so switching details can't misdirect a draft.
    comment_drafts: HashMap<String, String>,
    /// `DropStash` index armed by a first tap; a second tap sends it.
    stash_drop_confirm: Option<usize>,
    /// `DropStash` request in flight — drop buttons serialize on it so a
    /// double tap cannot delete the stash that slides into the index.
    stash_drop_inflight: Option<usize>,
    models: Vec<ComposerModel>,
    session_drafts: Vec<SessionInfo>,
    selected_model: Option<String>,
    effort: Option<ReasoningEffort>,
    mode: OrchestratorMode,
    _link_task: Task<()>,
    _pump: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl MobileApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = cx.new(|cx| InputState::new(window, cx).placeholder("192.168.x.x"));
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("port"));
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("pairing token")
                .masked(true)
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search chats"));
        let git_message = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .placeholder("Commit message")
                .auto_grow(1, 4)
                .soft_wrap(true)
        });
        let git_branch =
            cx.new(|cx| InputState::new(window, cx).placeholder("New branch name"));
        let issue_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search issues"));
        let issue_title = cx.new(|cx| InputState::new(window, cx).placeholder("Issue title"));
        let issue_body = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .placeholder("Describe the issue")
                .auto_grow(2, 8)
                .soft_wrap(true)
        });
        let issue_comment = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .placeholder("Add a comment")
                .auto_grow(1, 4)
                .soft_wrap(true)
        });
        let pr_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search pull requests"));
        let pr_comment = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .placeholder("Add a comment")
                .auto_grow(1, 4)
                .soft_wrap(true)
        });
        let auto_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Automation name"));
        let auto_model = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Model, e.g. claude-sonnet-4-5")
        });
        let auto_effort =
            cx.new(|cx| InputState::new(window, cx).placeholder("Effort, e.g. medium"));
        let auto_minutes =
            cx.new(|cx| InputState::new(window, cx).placeholder("Minutes between runs"));
        let auto_prompt = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .placeholder("Prompt the run executes")
                .auto_grow(3, 10)
                .soft_wrap(true)
        });
        let composer = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .placeholder("Ask Threadlane to build or fix something")
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .soft_wrap(true)
        });
        let mut subscriptions = [
            &host,
            &port,
            &token,
            &search,
            &git_branch,
            &issue_search,
            &issue_title,
            &pr_search,
            &auto_name,
            &auto_model,
            &auto_effort,
            &auto_minutes,
        ]
        .iter()
            .map(|input| {
                cx.subscribe_in(input, window, |_, _, event, _, _| match event {
                    InputEvent::Focus => gpui_mobile::show_keyboard(),
                    InputEvent::Blur => gpui_mobile::hide_keyboard(),
                    _ => {}
                })
            })
            .collect::<Vec<_>>();
        subscriptions.push(
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::Focus => { gpui_mobile::show_keyboard(); cx.notify(); }
                    InputEvent::Blur => { gpui_mobile::hide_keyboard(); cx.notify(); }
                    InputEvent::PressEnter { .. } => this.submit_composer(window, cx),
                    _ => {}
                },
            ),
        );
        for entity in [&git_message, &issue_body, &issue_comment, &pr_comment, &auto_prompt] {
            subscriptions.push(
                cx.subscribe_in(entity, window, |_, _, event, _, _| {
                    match event {
                        InputEvent::Focus => gpui_mobile::show_keyboard(),
                        InputEvent::Blur => gpui_mobile::hide_keyboard(),
                        _ => {}
                    }
                }),
            );
        }

        subscriptions.push(
            cx.subscribe_in(&search, window, |this, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.client.search_query = input.read(cx).value().to_string();
                    this.refresh_session_rows();
                    cx.notify();
                }
            }),
        );

        // Poll for pairing deep links arriving while the app runs — the
        // UIKit handler runs outside GPUI and can only stash them.
        let link_task = cx.spawn_in(window, async move |this, cx| {
            // A link may have launched the app before this task started.
            if let Ok(Some(url)) = gpui_mobile::packages::deeplink::get_initial_link() {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.apply_pairing_link(&url, window, cx);
                });
            }
            let mut tick = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                tick += 1;
                let links = take_pending_links();
                let wake_reconnect =
                    FOREGROUND_RECONNECT_REQUESTED.swap(false, Ordering::SeqCst);
                // The daemon only publishes `ProjectChanged` on attach/detach
                // or when asked, so new sessions and renames inside an
                // attached project never reach the client — re-ask on a
                // slow cadence to keep the list current.
                let refresh_projects = tick % 30 == 0;
                if links.is_empty() && !refresh_projects && !wake_reconnect {
                    continue;
                }
                let _ = this.update_in(cx, |this, window, cx| {
                    for url in links {
                        this.apply_pairing_link(&url, window, cx);
                    }
                    if wake_reconnect {
                        if let Some(daemon) = &this.daemon {
                            daemon.request_reconnect();
                        }
                    }
                    if refresh_projects {
                        if let Some(daemon) = &this.daemon {
                            if daemon.is_connected() {
                                daemon.send(SessionCommand::GetProjects);
                                if let Some(work_dir) = &this.client.sidebar_project_filter {
                                    daemon.request(SessionCommand::GitRequest {
                                        work_dir: work_dir.clone(),
                                        operation: GitOperation::Inspect { sync_remote: false },
                                    });
                                }
                            }
                        }
                    }
                });
            }
        });

        let (saved_device_list, saved_devices_error) = match restore_saved_devices() {
            Ok(devices) => (devices, None),
            Err(error) => (SavedDeviceList::default(), Some(error)),
        };
        let selected_device_id = saved_device_list.selected_id().map(str::to_string);
        let mut app = Self {
            screen: Screen::Connect,
            host,
            port,
            token,
            composer,
            search,
            question_inputs: HashMap::new(),
            connect_error: saved_devices_error,
            saved_device_list,
            remove_device_confirmation: None,
            selected_device_id,
            active_device_id: None,
            connection_snapshot: None,
            connection_generation: 0,
            removed_device_ids: HashSet::new(),
            sending: false,
            pending_composer: HashMap::new(),
            uncertain_prompts: HashSet::new(),
            daemon: None,
            link_state: "Disconnected".to_string(),
            seen_links: Vec::new(),
            client: ClientState::default(),
            markdown_states: HashMap::new(),
            active: None,
            sessions_list: ListState::new(0, ListAlignment::Top, window.rem_size() * 4.5),
            session_rows: Vec::new(),
            project_git: HashMap::new(),
            tab: Tab::Chats,
            sidebar_open: false,
            git_message,
            git_branch,
            git_diff: None,
            git_diff_pending: None,
            git_expanded: HashSet::new(),
            git_object_files: HashMap::new(),
            issues: None,
            issue_filter: GitHubIssueListState::Open,
            issue_search,
            issue_detail: None,
            issue_detail_pending: None,
            issue_confirm_delete: false,
            issue_comment,
            new_issue_open: false,
            issue_title,
            issue_body,
            prs: None,
            pr_filter: GitHubPrListState::Open,
            pr_search,
            pr_detail: None,
            pr_detail_pending: None,
            pr_comment,
            auto_editing: None,
            auto_name,
            auto_model,
            auto_effort,
            auto_minutes,
            auto_prompt,
            auto_schedule: AutoSchedule::Interval,
            auto_worktree: true,
            auto_enabled: true,
            auto_delete_confirm: None,
            pending_queued_cancel: None,
            auto_run_delete_confirm: None,
            comment_drafts: HashMap::new(),
            stash_drop_confirm: None,
            stash_drop_inflight: None,
            models: Vec::new(),
            session_drafts: Vec::new(),
            selected_model: None,
            effort: None,
            mode: OrchestratorMode::Normal,
            _link_task: link_task,
            _pump: None,
            _subscriptions: subscriptions,
        };
        // Restore the selected desktop; other saved credentials remain
        // available but are never tried as fallbacks.
        if let Some(selected_id) = app.selected_device_id.clone() {
            app.connect_saved_device(&selected_id, window, cx);
        }
        app
    }

    /// Parse `threadlane://pair?host=…&port=…&token=…` and connect.
    fn apply_pairing_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.seen_links.iter().any(|seen| seen == url) {
            return;
        }
        let Ok(parsed) = url::Url::parse(url) else {
            return;
        };
        if parsed.scheme() != "threadlane" || parsed.host_str() != Some("pair") {
            return;
        }
        self.seen_links.push(url.to_string());
        let query = |key: &str| {
            parsed
                .query_pairs()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.to_string())
        };
        // A link that redirects to a different endpoint but carries no token
        // must not inherit the credential saved for another host — the Bearer
        // token is only valid for the endpoint it was paired with.
        let endpoint_changed = query("host")
            .is_some_and(|host| host != self.host.read(cx).value().trim().to_owned())
            || query("port")
                .is_some_and(|port| port != self.port.read(cx).value().trim().to_owned());
        if endpoint_changed && query("token").is_none() {
            self.token
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if let Some(host) = query("host") {
            self.host
                .update(cx, |input, cx| input.set_value(host, window, cx));
        }
        if let Some(port) = query("port") {
            self.port
                .update(cx, |input, cx| input.set_value(port, window, cx));
        }
        if let Some(token) = query("token") {
            self.token
                .update(cx, |input, cx| input.set_value(token, window, cx));
        }
        self.start_connection(
            ConnectionSnapshot {
                device_id: query("device_id"),
                name: query("name"),
                host: self.host.read(cx).value().trim().to_owned(),
                port: self
                    .port
                    .read(cx)
                    .value()
                    .trim()
                    .parse()
                    .unwrap_or_default(),
                token: self.token.read(cx).value().trim().to_owned(),
            },
            window,
            cx,
        );
    }

    fn connect_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.host.read(cx).value().trim().to_owned();
        let port = self.port.read(cx).value().trim().to_owned();
        let token = self.token.read(cx).value().trim().to_owned();
        let Ok(port_number) = port.parse::<u16>() else {
            self.connect_error =
                Some("Enter a valid port shown in the desktop QR dialog".into());
            cx.notify();
            return;
        };
        if host.is_empty() {
            self.connect_error = Some("Enter the host shown in the desktop QR dialog".into());
            cx.notify();
            return;
        }
        let existing = self.saved_device_list.rows().into_iter().find_map(|row| {
            self.saved_device_list.device(&row.id).filter(|device| {
                device.host == host && device.port == port_number && device.token() == token
            })
        });
        self.start_connection(
            ConnectionSnapshot {
                device_id: existing.as_ref().map(|device| device.id.clone()),
                name: existing.map(|device| device.name),
                host,
                port: port_number,
                token,
            },
            window,
            cx,
        );
    }

    pub fn saved_devices(&self) -> Vec<SavedDeviceInfo> {
        self.saved_device_list.rows()
    }

    pub fn selected_device_id(&self) -> Option<&str> {
        self.selected_device_id.as_deref()
    }

    pub fn current_device_id(&self) -> Option<&str> {
        self.active_device_id.as_deref()
    }

    pub fn connect_saved_device(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(device) = self.saved_device_list.device(id) else {
            self.connect_error = Some("Saved desktop was removed".into());
            cx.notify();
            return;
        };
        let token = device.token().to_string();
        self.selected_device_id = Some(id.to_string());
        self.saved_device_list.select(id);
        if let Err(error) = persist_saved_devices(&self.saved_device_list) {
            self.connect_error = Some(error);
        }
        self.host
            .update(cx, |input, cx| input.set_value(device.host.clone(), window, cx));
        self.port.update(cx, |input, cx| {
            input.set_value(device.port.to_string(), window, cx)
        });
        self.token
            .update(cx, |input, cx| input.set_value(device.token().to_string(), window, cx));
        self.start_connection(
            ConnectionSnapshot {
                device_id: Some(device.id),
                name: Some(device.name),
                host: device.host,
                port: device.port,
                token,
            },
            window,
            cx,
        );
    }

    pub fn remove_saved_device(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let device = self
            .saved_device_list
            .device(id)
            .ok_or_else(|| "saved desktop not found".to_string())?;
        let mut updated = self.saved_device_list.clone();
        updated.remove(id);
        persist_saved_devices(&updated)?;
        self.saved_device_list = updated;
        self.removed_device_ids.insert(id.to_string());
        if self.selected_device_id.as_deref() == Some(id) {
            self.selected_device_id = None;
        }
        self.seen_links.retain(|link| {
            let Ok(parsed) = url::Url::parse(link) else {
                return true;
            };
            let value = |key: &str| {
                parsed
                    .query_pairs()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value.to_string())
            };
            !(value("device_id").as_deref() == Some(id)
                || (value("host").as_deref() == Some(device.host.as_str())
                    && value("port").as_deref() == Some(device.port.to_string().as_str())
                    && value("token").as_deref() == Some(device.token())))
        });
        if self.active_device_id.as_deref() == Some(id)
            || self
                .connection_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.device_id.as_deref() == Some(id))
        {
            self.disconnect(cx);
            self.connect_error = None;
            self.token.update(cx, |input, cx| input.set_value("", window, cx));
            cx.notify();
        }
        Ok(())
    }

    fn start_connection(
        &mut self,
        snapshot: ConnectionSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if snapshot.host.is_empty() || snapshot.port == 0 {
            self.connect_error =
                Some("Enter the host and port shown in the desktop QR dialog".into());
            cx.notify();
            return;
        }
        if snapshot.token.is_empty() {
            self.connect_error = Some("Pairing token is empty; scan a new desktop invitation".into());
            cx.notify();
            return;
        }
        let url = format!("ws://{}:{}", snapshot.host, snapshot.port);
        let daemon = match MobileDaemon::connect_named(
            url,
            Some(snapshot.token.clone()),
            Some(local_device_name()),
        ) {
            Ok(daemon) => daemon,
            Err(error) => {
                self.connect_error = Some(error);
                cx.notify();
                return;
            }
        };
        if let Some(id) = &snapshot.device_id {
            self.removed_device_ids.remove(id);
        }
        self.connection_generation = self.connection_generation.wrapping_add(1);
        let generation = self.connection_generation;
        self.link_state = "Connecting…".to_string();
        self.connect_error = None;
        self.active_device_id = None;
        self.selected_device_id = snapshot.device_id.clone();
        self.connection_snapshot = Some(snapshot);
        self.client.projects.clear();
        self.session_drafts.clear();
        self.project_git.clear();
        self.models.clear();
        self.selected_model = None;
        self.effort = None;
        self.active = None;
        self.git_diff = None;
        self.git_diff_pending = None;
        self.git_expanded.clear();
        self.git_object_files.clear();
        self.issues = None;
        self.issue_detail = None;
        self.issue_detail_pending = None;
        self.new_issue_open = false;
        self.prs = None;
        self.pr_detail = None;
        self.pr_detail_pending = None;
        self.comment_drafts.clear();
        self.stash_drop_confirm = None;
        self.stash_drop_inflight = None;
        self.daemon = Some(daemon);
        self.screen = Screen::Main;
        self.tab = Tab::Chats;
        self.sidebar_open = false;

        // Pump the wire onto the view until the daemon is dropped.
        let mut events = self.daemon.as_mut().and_then(|daemon| daemon.take_events());
        self._pump = Some(cx.spawn_in(window, async move |this, cx| {
            let Some(events) = events.as_mut() else {
                return;
            };
            while let Some(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        if this.connection_generation == generation {
                            this.apply_event(event, window, cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        // Dropping the client closes its command channel and ends the
        // driver's reconnect loop.
        self.daemon = None;
        self._pump = None;
        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.active_device_id = None;
        self.connection_snapshot = None;
        self.sending = false;
        self.pending_composer.clear();
        self.client.projects.clear();
        self.active = None;
        self.client.pending_permissions.clear();
        self.client.pending_questions.clear();
        self.client.queued_questions.clear();
        self.link_state = "Disconnected".to_string();
        self.tab = Tab::Chats;
        self.sidebar_open = false;
        self.git_diff = None;
        self.git_diff_pending = None;
        self.git_expanded.clear();
        self.git_object_files.clear();
        self.issues = None;
        self.issue_detail = None;
        self.issue_detail_pending = None;
        self.new_issue_open = false;
        self.prs = None;
        self.pr_detail = None;
        self.pr_detail_pending = None;
        self.comment_drafts.clear();
        self.stash_drop_confirm = None;
        self.stash_drop_inflight = None;
        self.screen = Screen::Connect;
        cx.notify();
    }

    fn open_session(&mut self, info: &SessionInfo, window: &mut Window, cx: &mut Context<Self>) {
        if !self.session_drafts.iter().any(|draft| draft.id == info.id) {
            if let Some(daemon) = &self.daemon {
                daemon.send(SessionCommand::GetSessionSnapshot {
                    session_id: info.id.clone(),
                });
            }
        }
        self.request_composer(SessionCommand::GetComposerOptions {
            work_dir: info.work_dir.clone(),
            session_id: Some(info.id.clone()),
        });
        self.models.clear();
        self.selected_model = None;
        self.effort = None;
        self.markdown_states.clear();
        self.client.select_session(info);
        let key = (
            self.client.active_work_dir.clone(),
            self.client.active_session_id.clone(),
        );
        let draft = self.client.composer_drafts.remove(&key).unwrap_or_default();
        self.composer
            .update(cx, |input, cx| input.set_value(draft.text, window, cx));
        self.active = Some(ActiveSession::new(info, window));
        self.tab = Tab::Chats;
        cx.notify();
    }

    fn begin_session(&mut self, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        if self.active.is_some() {
            self.client.composer_drafts.insert(
                (
                    self.client.active_work_dir.clone(),
                    self.client.active_session_id.clone(),
                ),
                threadlane_client::ComposerDraft {
                    text: self.composer.read(cx).value().to_string(),
                    images: Vec::new(),
                },
            );
        }
        let project = self
            .client
            .sidebar_project_filter
            .clone()
            .or_else(|| self.client.active_work_dir.clone())
            .or_else(|| {
                self.client
                    .projects
                    .first()
                    .map(|project| project.work_dir.clone())
            });
        if let (Some(work_dir), Some(daemon)) = (project, &self.daemon) {
            daemon.request(SessionCommand::BeginSession { work_dir });
            self.sending = true;
        }
        cx.notify();
    }

    fn apply_response(
        &mut self,
        command: &SessionCommand,
        response: CommandResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match response {
            CommandResponse::SessionDraft { session } => {
                self.sending = false;
                self.markdown_states.clear();
                self.models.clear();
                self.selected_model = None;
                self.effort = None;
                let project_draft = self.client.composer_drafts.remove(&(Some(session.work_dir.clone()), None));
                // Changing a draft's project replaces its temporary session, keeping the prompt.
                if project_draft.is_some() {
                    if let Some(active) = &self.active {
                        if self.session_drafts.iter().any(|draft| draft.id == active.id) {
                            self.session_drafts.retain(|draft| draft.id != active.id);
                            for project in &mut self.client.projects {
                                project.sessions.retain(|draft| draft.id != active.id);
                            }
                        }
                    }
                }
                // A draft has no transcript until its first accepted prompt.
                self.session_drafts.push(session.clone());
                if let Some(project) = self
                    .client
                    .projects
                    .iter_mut()
                    .find(|project| project.work_dir == session.work_dir)
                {
                    project.sessions.push(session.clone());
                }
                self.refresh_session_rows();
                self.client.select_session(&session);
                self.client.messages = Default::default();
                let draft = project_draft.unwrap_or_default();
                self.composer
                    .update(cx, |input, cx| input.set_value(draft.text, window, cx));
                self.active = Some(ActiveSession::new(&session, window));
                self.tab = Tab::Chats;
                self.request_composer(SessionCommand::GetComposerOptions {
                    work_dir: session.work_dir,
                    session_id: Some(session.id),
                });
            }
            CommandResponse::ComposerOptions {
                models,
                model,
                effort,
                mode,
            } => {
                if let SessionCommand::GetComposerOptions {
                    work_dir,
                    session_id,
                } = command
                {
                    if self.client.active_work_dir.as_ref() != Some(work_dir)
                        || &self.client.active_session_id != session_id
                    {
                        return;
                    }
                }
                self.models = models;
                self.selected_model = Some(model);
                self.effort = Some(effort);
                self.mode = mode;
            }
            CommandResponse::Automation { response } => {
                let AutomationResponse::Projection { projection } = response;
                // GetSnapshot doubles as the (re)connect baseline — reconcile
                // it exactly like an AutomationChanged event so the pending
                // permission/question surfaces stay attached to the run.
                let commands = self
                    .client
                    .apply_event(SessionEvent::AutomationChanged { projection });
                if let Some(daemon) = &self.daemon {
                    for command in commands {
                        daemon.send(command);
                    }
                }
                if matches!(
                    command,
                    SessionCommand::AutomationRequest {
                        command: AutomationCommand::Save { .. }
                    }
                ) {
                    self.auto_editing = None;
                }
            }
            CommandResponse::GitHub { response } => {
                let SessionCommand::GitHubRequest { work_dir, operation } = command else {
                    return;
                };
                // Replies are scoped to the project filter they were
                // issued under; a reply from a previous pick must not
                // land on the current panel.
                if self.client.sidebar_project_filter.as_ref() != Some(work_dir) {
                    return;
                }
                match response {
                    GitHubResponse::Issues { issues } => {
                        if matches!(operation, GitHubOperation::ListIssues { .. }) {
                            self.issues = Some(issues);
                        }
                    }
                    GitHubResponse::Issue { detail } => {
                        let number = detail.summary.issue.number;
                        if self.issue_detail_pending == Some(number) {
                            self.issue_detail_pending = None;
                            self.stash_comment_drafts(cx);
                            self.restore_comment_draft("issue", number, window, cx);
                            self.issue_detail = Some(detail);
                        }
                    }
                    GitHubResponse::PullRequests { prs } => {
                        if matches!(operation, GitHubOperation::ListPullRequests { .. }) {
                            self.prs = Some(prs);
                        }
                    }
                    GitHubResponse::PullRequest { pr } => {
                        if self.pr_detail_pending == Some(pr.number) {
                            self.pr_detail_pending = None;
                            self.stash_comment_drafts(cx);
                            self.restore_comment_draft("pr", pr.number, window, cx);
                            self.pr_detail = Some(pr);
                        }
                    }
                    GitHubResponse::Text { text } => {
                        if let Some(title) = self.git_diff_pending.take() {
                            self.git_diff = Some((title, text));
                        }
                    }
                    GitHubResponse::Number { number } => {
                        // Only now is the issue durable — the form's text
                        // survived any transport/auth failure until here.
                        self.issue_title.update(cx, |input, cx| {
                            input.set_value("", window, cx)
                        });
                        self.issue_body.update(cx, |input, cx| {
                            input.set_value("", window, cx)
                        });
                        self.new_issue_open = false;
                        self.client.session_status = Some(format!("Created issue #{number}"));
                        self.refresh_issues();
                        self.send_github(GitHubOperation::InspectIssue { number });
                        self.issue_detail_pending = Some(number);
                    }
                    GitHubResponse::Action { message } => {
                        if let Some(message) = message {
                            self.client.session_status = Some(message);
                        }
                        // Refresh whatever a mutation may have touched: the
                        // list, and the open detail's comment/state.
                        match operation {
                            GitHubOperation::CommentIssue { number, .. }
                            | GitHubOperation::SetIssueState { number, .. } => {
                                if matches!(operation, GitHubOperation::CommentIssue { .. }) {
                                    self.comment_drafts
                                        .remove(&format!("{}|issue-{number}", work_dir.display()));
                                }
                                if let Some(detail) = &self.issue_detail {
                                    let number = detail.summary.issue.number;
                                    self.issue_detail_pending = Some(number);
                                    self.send_github(GitHubOperation::InspectIssue { number });
                                }
                                self.refresh_issues();
                            }
                            GitHubOperation::DeleteIssue { number, .. } => {
                                self.issue_detail = None;
                                self.comment_drafts
                                    .remove(&format!("{}|issue-{number}", work_dir.display()));
                                self.refresh_issues();
                            }
                            GitHubOperation::CommentPullRequest { number, .. } => {
                                self.comment_drafts
                                    .remove(&format!("{}|pr-{number}", work_dir.display()));
                                if let Some(pr) = &self.pr_detail {
                                    let number = pr.number;
                                    self.pr_detail_pending = Some(number);
                                    self.send_github(GitHubOperation::InspectPullRequest {
                                        number,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            CommandResponse::Git { response } => {
                let SessionCommand::GitRequest { work_dir, operation } = command else {
                    return;
                };
                match response {
                    GitResponse::Status { status } => {
                        self.project_git.insert(work_dir.clone(), *status);
                        self.refresh_session_rows();
                    }
                    GitResponse::Action { outcome } => {
                        if matches!(operation, GitOperation::DropStash { .. }) {
                            self.stash_drop_inflight = None;
                            self.stash_drop_confirm = None;
                        }
                        if let Some(error) = &outcome.action_error {
                            self.client.session_status = Some(error.clone());
                        } else if let Some(message) = &outcome.message {
                            self.client.session_status = Some(message.clone());
                        }
                        if let Ok(status) = outcome.status {
                            self.project_git.insert(work_dir.clone(), status);
                            self.refresh_session_rows();
                        }
                        if outcome.action_error.is_none()
                            && matches!(operation, GitOperation::Commit { .. })
                        {
                            self.git_message.update(cx, |input, cx| {
                                input.set_value("", window, cx)
                            });
                        }
                    }
                    GitResponse::Files { files } => {
                        let key = match operation {
                            GitOperation::CommitFiles { sha } => Some(format!("commit-{sha}")),
                            GitOperation::StashFiles { index } => Some(format!("stash-{index}")),
                            _ => None,
                        };
                        if let Some(key) = key {
                            // Scope by the request's own project: index
                            // keys repeat across projects and a late reply
                            // must not land under another project's list.
                            self.git_object_files
                                .insert(format!("{}|{key}", work_dir.display()), files);
                        }
                    }
                    GitResponse::Text { text } => {
                        if let Some(title) = self.git_diff_pending.take() {
                            self.git_diff = Some((title, text));
                        }
                    }
                    _ => {}
                }
            }
            CommandResponse::CancelledQueuedMessage {
                session_id,
                entry_id,
                ..
            } => {
                let echo_id = format!("queued-user-{session_id}-{entry_id}");
                self.client.messages_mut().retain(|m| m.id != echo_id);
                if self
                    .pending_queued_cancel
                    .as_ref()
                    .is_some_and(|(sid, eid)| sid == &session_id && eid == &entry_id)
                {
                    self.pending_queued_cancel = None;
                }
                self.client.session_status = Some("Queued message removed".to_string());
            }
            CommandResponse::Ack => {
                let session_id = match command {
                    SessionCommand::SetModel { session_id, .. }
                    | SessionCommand::SetReasoningEffort { session_id, .. }
                    | SessionCommand::SetOrchestratorMode { session_id, .. } => Some(session_id),
                    _ => None,
                };
                if session_id.is_some_and(|id| self.client.active_session_id.as_ref() != Some(id)) {
                    return;
                }
                match command {
                    SessionCommand::SetModel { model, .. } => {
                        self.selected_model = Some(model.clone());
                        if let Some(efforts) = self
                            .models
                            .iter()
                            .find(|m| m.id == *model)
                            .map(|m| &m.efforts)
                        {
                            if !self.effort.is_some_and(|effort| efforts.contains(&effort)) {
                                self.effort = efforts.first().copied();
                            }
                        }
                    }
                    SessionCommand::SetReasoningEffort { effort, .. } => {
                        self.effort = Some(*effort)
                    }
                    SessionCommand::SetOrchestratorMode { mode, .. } => self.mode = *mode,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn request_composer(&mut self, command: SessionCommand) {
        if let Some(daemon) = &self.daemon {
            if let Some(id) = composer_command_session(&command) {
                *self.pending_composer.entry(id.to_owned()).or_default() += 1;
            }
            daemon.request(command);
        }
    }

    fn composer_pending(&self) -> bool {
        self.client
            .active_session_id
            .as_ref()
            .is_some_and(|id| self.pending_composer.get(id).copied().unwrap_or_default() > 0)
    }

    fn composer_project(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.client.active_work_dir.clone();
        let label = self
            .client
            .projects
            .iter()
            .find(|project| Some(&project.work_dir) == selected.as_ref())
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "Project".into());
        let draft = self.active.as_ref().is_some_and(|active| {
            self.session_drafts
                .iter()
                .any(|draft| draft.id == active.id)
        });
        let projects = self.client.projects.clone();
        let entity = cx.entity();
        Button::new("composer-project")
            .ghost()
            .h_11()
            .w_full()
            .min_w_0()
            .child(div().flex().items_center().gap_1().min_w_0().text_sm()
                .child(div().min_w_0().truncate().child(label.clone()))
                .when(draft, |this| this.child(div().flex_none().child("▾"))))
            .accessibility_label(format!("Project: {label}"))
            .disabled(
                !draft
                    || self.sending
                    || !self
                        .daemon
                        .as_ref()
                        .is_some_and(|daemon| daemon.is_connected()),
            )
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                projects
                    .iter()
                    .fold(menu.scrollable(true), |menu, project| {
                        let entity = entity.clone();
                        let work_dir = project.work_dir.clone();
                        let current = Some(&work_dir) == selected.as_ref();
                        menu.item(
                            PopupMenuItem::new(format!(
                                "{}{}",
                                if current { "✓ " } else { "" },
                                project.name
                            ))
                            .checked(current)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    let draft = this.active.as_ref().is_some_and(|active| {
                                        this.session_drafts
                                            .iter()
                                            .any(|draft| draft.id == active.id)
                                    });
                                    if current || !draft || this.sending {
                                        return;
                                    }
                                    if let Some(daemon) = &this.daemon {
                                        this.client.composer_drafts.insert(
                                            (Some(work_dir.clone()), None),
                                            threadlane_client::ComposerDraft {
                                                text: this.composer.read(cx).value().to_string(),
                                                images: Vec::new(),
                                            },
                                        );
                                        daemon.request(SessionCommand::BeginSession {
                                            work_dir: work_dir.clone(),
                                        });
                                        this.sending = true;
                                        cx.notify();
                                    }
                                });
                            }),
                        )
                    })
            })
            .into_any_element()
    }

    fn composer_model_label(&self) -> String {
        self.models
            .iter()
            .find(|model| Some(&model.id) == self.selected_model.as_ref())
            .map(|model| model.label.clone())
            .or_else(|| {
                self.selected_model
                    .clone()
                    .filter(|model| !model.is_empty())
            })
            .unwrap_or_else(|| "No model available".into())
    }

    fn open_configuration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        let session_id = self.client.active_session_id.clone();
        window.open_sheet_at(Placement::Bottom, cx, move |sheet, _, cx| {
            let content = entity.update(cx, |this, cx| {
                if this.client.active_session_id == session_id {
                    this.composer_options(cx)
                } else {
                    div()
                        .child("Chat changed. Close configuration to continue.")
                        .into_any_element()
                }
            });
            sheet
                .title("Configuration")
                .size(rems(26.0))
                .max_h_full()
                .resizable(false)
                .child(content)
                .footer(
                    Button::new("configuration-done")
                        .ghost()
                        .h_11()
                        .label("Done")
                        .on_click(|_, window, cx| window.close_sheet(cx)),
                )
        });
    }

    fn composer_options(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let models = self.models.clone();
        let selected = self.selected_model.clone();
        let model_label = self.composer_model_label();
        let efforts = models
            .iter()
            .find(|m| Some(&m.id) == selected.as_ref())
            .map(|m| m.efforts.clone())
            .unwrap_or_default();
        let effort = self.effort;
        let mode = self.mode;
        let enabled = self.daemon.as_ref().is_some_and(|d| d.is_connected())
            && !self.client.is_generating
            && !self.composer_pending();
        let model_entity = entity.clone();
        let effort_entity = entity.clone();
        // iOS has no path-based icon source; text indicators keep choices visible.
        div().flex().flex_col().w_full().gap_3()
            .when(models.is_empty(), |this| this.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Connect a provider in desktop Settings to choose a model.")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Model"))
            .child(Button::new("mobile-model").ghost().h_11().w_full().child(div().min_w_0().truncate().child(format!("{model_label} ▾")))
                .accessibility_label("Select model").dropdown_caret(true).disabled(!enabled || models.is_empty())
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                    models.iter().fold(menu.scrollable(true), |menu, model| {
                        let entity = model_entity.clone(); let id = model.id.clone();
                        menu.item(PopupMenuItem::new(format!("{}{}", if Some(&model.id) == selected.as_ref() { "✓ " } else { "" }, model.label)).checked(Some(&model.id) == selected.as_ref())
                            .on_click(move |_, _, cx| { entity.update(cx, |this, cx| {
                                if !this.composer_pending() {
                                    if let Some(active) = &this.active {
                                        let session_id = active.id.clone();
                                        this.request_composer(SessionCommand::SetModel { session_id, model: id.clone() });
                                    }
                                } cx.notify();
                            }); }))
                    })
                }))
            .child(div().flex().flex_col().gap_3()
                .when(!efforts.is_empty(), |this| this
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Reasoning effort"))
                    .child(
                    Button::new("mobile-effort").ghost().h_11().w_full().label(format!("{} ▾", effort.map(|e| e.label()).unwrap_or("Effort")))
                        .accessibility_label("Reasoning effort").dropdown_caret(true).disabled(!enabled)
                        .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                            efforts.iter().fold(menu, |menu, value| {
                                let entity = effort_entity.clone(); let value = *value;
                                menu.item(PopupMenuItem::new(format!("{}{}", if Some(value) == effort { "✓ " } else { "" }, value.label())).checked(Some(value) == effort)
                                    .on_click(move |_, _, cx| { entity.update(cx, |this, cx| {
                                        if !this.composer_pending() {
                                            if let Some(active) = &this.active {
                                                let session_id = active.id.clone();
                                                this.request_composer(SessionCommand::SetReasoningEffort { session_id, effort: value });
                                            }
                                        } cx.notify();
                                    }); }))
                            })
                        })))
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Mode"))
                .child(Button::new("mobile-mode").ghost().h_11().w_full().label(format!("{} ▾", mode.label())).accessibility_label("Agent mode")
                    .dropdown_caret(true).disabled(!enabled)
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        [OrchestratorMode::Normal, OrchestratorMode::Fusion].into_iter().fold(menu, |menu, value| {
                            let entity = entity.clone();
                            menu.item(PopupMenuItem::new(format!("{}{}", if value == mode { "✓ " } else { "" }, value.label())).checked(value == mode)
                                .on_click(move |_, _, cx| { entity.update(cx, |this, cx| {
                                    if !this.composer_pending() {
                                        if let Some(active) = &this.active {
                                            let session_id = active.id.clone();
                                            this.request_composer(SessionCommand::SetOrchestratorMode { session_id, mode: value });
                                        }
                                    } cx.notify();
                                }); }))
                        })
                    })))
            .into_any_element()
    }

    fn apply_event(&mut self, event: MobileEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let MobileEvent::CommandResult { command, .. } = &event {
            if let Some(id) = composer_command_session(command) {
                if let Some(count) = self.pending_composer.get_mut(id) {
                    *count = count.saturating_sub(1);
                }
            }
        }
        match event {
            MobileEvent::Connected => {
                if let Some(daemon) = &self.daemon {
                    daemon.send(SessionCommand::GetProjects);
                    if let Some(active) = &self.active {
                        daemon.send(SessionCommand::GetSessionSnapshot {
                            session_id: active.id.clone(),
                        });
                    }
                }
                if let (Some(work_dir), Some(session_id)) = (
                    self.client.active_work_dir.clone(),
                    self.client.active_session_id.clone(),
                ) {
                    self.selected_model = None;
                    self.request_composer(SessionCommand::GetComposerOptions {
                        work_dir,
                        session_id: Some(session_id),
                    });
                }
                self.link_state = "Live".to_string();
                self.connect_error = None;
                if let Some(snapshot) = self.connection_snapshot.clone() {
                    let id = self
                        .daemon
                        .as_ref()
                        .and_then(MobileDaemon::paired_device_id)
                        .or_else(|| snapshot.device_id.clone())
                        .unwrap_or_else(|| {
                            format!("desktop-{}-{}", snapshot.host, snapshot.port)
                        });
                    if !self.removed_device_ids.contains(&id) {
                        let name = snapshot
                            .name
                            .clone()
                            .filter(|name| name != "New device")
                            .or_else(|| {
                                self.saved_device_list
                                    .device(&id)
                                    .map(|device| device.name)
                            })
                            .unwrap_or_else(|| format!("Desktop ({})", snapshot.host));
                        let saved = SavedDevice::new(
                            id.clone(),
                            name,
                            snapshot.host.clone(),
                            snapshot.port,
                            snapshot.token.clone(),
                        );
                        let previous_id = snapshot.device_id.as_deref().filter(|previous_id| {
                            self.saved_device_list
                                .device(previous_id)
                                .is_some_and(|device| device.token() == snapshot.token.as_str())
                        });
                        if let Some(previous_id) = previous_id {
                            self.saved_device_list
                                .replace_identity(previous_id, saved, true);
                        } else {
                            self.saved_device_list.upsert(saved, true);
                        }
                        if let Some(snapshot) = &mut self.connection_snapshot {
                            snapshot.device_id = Some(id.clone());
                        }
                        self.selected_device_id = Some(id.clone());
                        self.active_device_id = Some(id);
                        match persist_saved_devices(&self.saved_device_list) {
                            Ok(()) => {
                                clear_legacy_pairing();
                            }
                            Err(error) => {
                                self.connect_error =
                                    Some(format!("Connected, but could not save this desktop: {error}"));
                            }
                        }
                    }
                }
            }
            MobileEvent::Connecting => {
                self.link_state = "Connecting…".into();
            }
            MobileEvent::Reconnecting => {
                self.link_state = "Reconnecting…".to_string();
            }
            MobileEvent::Fatal(error) => {
                self.link_state = "Failed".to_string();
                self.connect_error = Some(error);
                self.screen = Screen::Connect;
            }
            MobileEvent::CommandResult { command, result } => match result {
                Ok(response) => {
                    self.apply_response(&command, response, window, cx);
                    match command {
                        SessionCommand::SubmitPrompt {
                            session_id, text, ..
                        } => {
                            self.sending = false;
                            if let Some(daemon) = &self.daemon {
                                daemon.send(SessionCommand::GetProjects);
                            }
                            let accepted_id =
                                format!("sent-user-{session_id}-{}", self.client.messages.len());
                            if let Some(message) = self
                                .client
                                .messages_mut()
                                .iter_mut()
                                .find(|m| m.id == format!("pending-user-{session_id}"))
                            {
                                message.id = accepted_id;
                            }
                            let key = (
                                self.client
                                    .projects
                                    .iter()
                                    .find(|p| p.sessions.iter().any(|s| s.id == session_id))
                                    .map(|p| p.work_dir.clone()),
                                Some(session_id.clone()),
                            );
                            if self.client.active_session_id.as_deref() == Some(&session_id)
                                && self.composer.read(cx).value().trim() == text
                            {
                                self.composer
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            }
                            if self
                                .client
                                .composer_drafts
                                .get(&key)
                                .is_some_and(|d| d.text.trim() == text)
                            {
                                self.client.composer_drafts.remove(&key);
                            }
                        }
                        SessionCommand::AnswerPermission {
                            session_id,
                            request_id,
                            ..
                        } => {
                            if self
                                .client
                                .pending_permissions
                                .get(&session_id)
                                .is_some_and(|r| r.id == request_id)
                            {
                                self.client.pending_permissions.remove(&session_id);
                            }
                        }
                        SessionCommand::AnswerQuestion { session_id, answer } => {
                            if self
                                .client
                                .pending_questions
                                .get(&session_id)
                                .is_some_and(|r| r.id == answer.request_id)
                            {
                                self.client.pop_question(&session_id);
                            }
                            let prefix = format!("{}\0", answer.request_id);
                            if let Some(active) = &mut self.active {
                                if active.id == session_id {
                                    active.answers.retain(|key, _| !key.starts_with(&prefix));
                                }
                            }
                            self.question_inputs
                                .retain(|key, _| !key.starts_with(&prefix));
                        }
                        _ => {}
                    }
                }
                Err(error) => {
                    if let SessionCommand::BeginSession { work_dir } = &command {
                        self.client.composer_drafts.remove(&(Some(work_dir.clone()), None));
                    }
                    if matches!(
                        command,
                        SessionCommand::SubmitPrompt { .. } | SessionCommand::BeginSession { .. }
                    ) {
                        self.sending = false;
                    }
                    if let SessionCommand::SubmitPrompt { session_id, .. } = &command {
                        if error
                            .starts_with(threadlane_client::RemoteDaemon::UNKNOWN_REQUEST_OUTCOME)
                        {
                            // Dispatch may still be running. Keep the echo and draft,
                            // and require the user to check before submitting again.
                            self.uncertain_prompts.insert(session_id.clone());
                            cx.notify();
                            return;
                        }
                        // Drop whichever optimistic echo the failed submit
                        // left — `pending-user-*` or the unbound
                        // `queued-user-*` one a mid-turn send pushes.
                        self.client.messages_mut().retain(|m| {
                            m.id != format!("pending-user-{session_id}")
                                && m.id != format!("queued-user-{session_id}")
                        });
                        if let Some(active) = &mut self.active {
                            active.transcript.sync(
                                self.client.messages.clone(),
                                self.client.is_generating,
                                false,
                                true,
                            );
                        }
                    }
                    if matches!(
                        command,
                        SessionCommand::SetModel { .. }
                            | SessionCommand::SetReasoningEffort { .. }
                            | SessionCommand::SetOrchestratorMode { .. }
                    ) {
                        if let (Some(work_dir), Some(session_id)) = (
                            self.client.active_work_dir.clone(),
                            self.client.active_session_id.clone(),
                        ) {
                            self.selected_model = None;
                            self.request_composer(SessionCommand::GetComposerOptions {
                                work_dir,
                                session_id: Some(session_id),
                            });
                        }
                    }
                    if let SessionCommand::GitRequest { work_dir, operation } = &command {
                        self.git_diff_pending = None;
                        self.stash_drop_inflight = None;
                        // An Inspect failing after a status was cached is not
                        // worth surfacing — the panel keeps showing its last
                        // status. With nothing cached (a non-git or
                        // unreachable repo) the tab would stall on "Loading…"
                        // with no explanation, so surface it like every
                        // other op the user asked for.
                        if !matches!(operation, GitOperation::Inspect { .. })
                            || !self.project_git.contains_key(work_dir)
                        {
                            self.client.session_status = Some(error.clone());
                        }
                        return;
                    }
                    if let SessionCommand::GitHubRequest { .. } = &command {
                        self.git_diff_pending = None;
                        self.issue_detail_pending = None;
                        self.pr_detail_pending = None;
                        self.client.session_status = Some(error.clone());
                        return;
                    }
                    if let SessionCommand::AutomationRequest { .. } = &command {
                        self.client.session_status = Some(error.clone());
                        return;
                    }
                    if let SessionCommand::CancelQueuedMessage {
                        session_id,
                        entry_id,
                        ..
                    } = &command
                    {
                        // The echo stays: the entry never left the queue.
                        if self
                            .pending_queued_cancel
                            .as_ref()
                            .is_some_and(|(sid, eid)| sid == session_id && eid == entry_id)
                        {
                            self.pending_queued_cancel = None;
                        }
                        self.client.session_status =
                            Some(format!("Could not remove queued message: {error}"));
                        return;
                    }
                    self.connect_error = Some(error.clone());
                    self.client.session_status = Some(error);
                }
            },
            MobileEvent::Event(event) => self.apply_session_event(event, cx),
        }
        cx.notify();
    }

    fn apply_session_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        let projects_changed = matches!(
            event,
            SessionEvent::ProjectChanged { .. } | SessionEvent::SessionRemoved { .. }
        );
        let mut event = event;
        if let SessionEvent::SessionRemoved { session_id, .. } = &event {
            self.session_drafts.retain(|draft| draft.id != *session_id);
        }
        match &event {
            // Bind the optimistic `queued-user-{session}` echo to its durable
            // queue entry so the row's steer/edit/remove controls appear.
            SessionEvent::FollowUpQueued {
                session_id,
                entry_id,
            } => {
                let pending_id = format!("queued-user-{session_id}");
                if let Some(message) = self
                    .client
                    .messages_mut()
                    .iter_mut()
                    .find(|m| m.id == pending_id)
                {
                    message.id = format!("queued-user-{session_id}-{entry_id}");
                }
            }
            // The entry left the daemon's queue — ours or another client's
            // cancel: drop the retained echo and settle the parked intent.
            SessionEvent::QueuedEntryCancelled {
                session_id,
                entry_id,
                ..
            } => {
                let echo_id = format!("queued-user-{session_id}-{entry_id}");
                self.client.messages_mut().retain(|m| m.id != echo_id);
                if self
                    .pending_queued_cancel
                    .as_ref()
                    .is_some_and(|(sid, eid)| sid == session_id && eid == entry_id)
                {
                    self.pending_queued_cancel = None;
                    self.client.session_status = Some("Queued message removed".to_string());
                }
            }
            _ => {}
        }
        if let SessionEvent::ProjectChanged { project } = &mut event {
            self.session_drafts.retain(|draft| {
                !project
                    .sessions
                    .iter()
                    .any(|session| session.id == draft.id)
            });
            project.sessions.extend(
                self.session_drafts
                    .iter()
                    .filter(|draft| draft.work_dir == project.work_dir)
                    .cloned(),
            );
            if !self.project_git.contains_key(&project.work_dir) {
                if let Some(daemon) = &self.daemon {
                    daemon.request(SessionCommand::GitRequest {
                        work_dir: project.work_dir.clone(),
                        operation: GitOperation::Inspect { sync_remote: false },
                    });
                }
            }
        }
        let commands = self.client.apply_event(event);
        if projects_changed {
            self.refresh_session_rows();
        }
        if let Some(daemon) = &self.daemon {
            for command in commands {
                daemon.send(command);
            }
        }
        if self.client.active_session_id.is_none() && self.active.is_some() {
            self.active = None;
        }
        if let Some(active) = &mut self.active {
            if let Some(info) = self
                .client
                .projects
                .iter()
                .flat_map(|p| &p.sessions)
                .find(|s| s.id == active.id)
            {
                if !info.title.trim().is_empty() {
                    active.title = info.title.clone();
                }
                active.git_branch = info.git_branch.clone();
                active.is_worktree = info.is_worktree;
                active.github_issue = info.github_issue.clone();
            }
            active.transcript.sync(
                self.client.messages.clone(),
                self.client.is_generating,
                false,
                true,
            );
        }
        cx.notify();
    }

    /// Submit normally; the daemon queues a prompt while a turn is running.
    fn submit_composer(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        if self.composer_pending()
            || self.selected_model.is_none()
            || self
                .client
                .active_session_id
                .as_ref()
                .is_some_and(|id| self.uncertain_prompts.contains(id))
        {
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        // Commands queued while the socket is down are drained without
        // delivery, so refuse unless a live socket can take it.
        let connected = self
            .daemon
            .as_ref()
            .is_some_and(|daemon| daemon.is_connected());
        if !connected {
            self.client.session_status = Some("Not connected — message was not sent".to_string());
            cx.notify();
            return;
        }
        let session_id = active.id.clone();
        if self.sending {
            return;
        }
        let command = SessionCommand::SubmitPrompt {
            session_id,
            work_dir: active.work_dir.clone(),
            text,
            images: Vec::new(),
            effort: self.effort,
            acp_config: Vec::new(),
            model: self.selected_model.clone(),
        };
        if let Some(daemon) = &self.daemon {
            // Mid-turn submissions queue as follow-ups; the unbound
            // `queued-user-{session}` echo is bound to its entry id by
            // `SessionEvent::FollowUpQueued`, which turns on the row's
            // steer/edit/remove controls.
            let echo_id = if self.client.is_generating {
                format!("queued-user-{}", active.id)
            } else {
                format!("pending-user-{}", active.id)
            };
            let draft = match &command {
                SessionCommand::SubmitPrompt { text, .. } => text.clone(),
                _ => unreachable!(),
            };
            self.client.messages_mut().push(ChatMessageInfo {
                id: echo_id,
                role: MessageRole::User,
                content: draft,
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
                retry_prompt: None,
            });
            active.transcript.sync(
                self.client.messages.clone(),
                self.client.is_generating,
                false,
                false,
            );
            active.transcript.list.scroll_to_end();
            daemon.request(command);
            self.sending = true;
            if self.client.is_generating {
                self.client.session_status = Some("Message queued…".to_string());
            }
        }
        cx.notify();
    }

    /// Mirrors `threadlane_acp_engine`'s `acp/` model-id prefix — ACP
    /// agents run external processes that cannot take a mid-turn steer,
    /// only queued follow-ups for the next turn.
    fn acp_selected(&self) -> bool {
        self.selected_model
            .as_deref()
            .is_some_and(|model| model.starts_with("acp/"))
    }

    /// Interrupt the live turn with the composer text instead of queueing
    /// behind it.
    fn steer_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() || !self.client.is_generating {
            return;
        }
        if self.acp_selected() {
            self.client.session_status = Some(
                "This agent does not support live steering. Use Queue for the next turn."
                    .to_string(),
            );
            cx.notify();
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        let connected = self
            .daemon
            .as_ref()
            .is_some_and(|daemon| daemon.is_connected());
        if !connected {
            self.client.session_status = Some("Not connected — message was not sent".to_string());
            cx.notify();
            return;
        }
        let session_id = active.id.clone();
        if let Some(daemon) = &self.daemon {
            daemon.send(SessionCommand::SteerMessage {
                session_id: session_id.clone(),
                text: text.clone(),
                images: Vec::new(),
            });
            let echo_id = format!(
                "steered-user-{session_id}-{}",
                self.client.messages.len()
            );
            self.client.messages_mut().push(ChatMessageInfo {
                id: echo_id,
                role: MessageRole::User,
                content: text,
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
                retry_prompt: None,
            });
            active.transcript.sync(
                self.client.messages.clone(),
                self.client.is_generating,
                false,
                false,
            );
            active.transcript.list.scroll_to_end();
            self.client.session_status = Some("Steering current turn…".to_string());
            self.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    /// Re-route a still-pending queued follow-up into the live turn.
    fn steer_queued(&mut self, entry_id: &str, cx: &mut Context<Self>) {
        if self.acp_selected() {
            self.client.session_status = Some(
                "This agent does not support live steering. Use Queue for the next turn."
                    .to_string(),
            );
            cx.notify();
            return;
        }
        let connected = self
            .daemon
            .as_ref()
            .is_some_and(|daemon| daemon.is_connected());
        if !connected {
            self.client.session_status = Some("Not connected — message was not sent".to_string());
            cx.notify();
            return;
        }
        let Some(session_id) = self.client.active_session_id.clone() else {
            return;
        };
        if let Some(daemon) = &self.daemon {
            daemon.send(SessionCommand::SteerQueuedMessage {
                session_id: session_id.clone(),
                entry_id: entry_id.to_string(),
            });
        }
        let queued_id = format!("queued-user-{session_id}-{entry_id}");
        if let Some(message) = self
            .client
            .messages_mut()
            .iter_mut()
            .find(|m| m.id == queued_id)
        {
            message.id = format!("steered-user-{session_id}-{entry_id}");
        }
        self.client.session_status = Some("Steering current turn…".to_string());
        cx.notify();
    }

    /// Drop a still-pending queued follow-up. `restore` puts the echo's
    /// staged text back into the composer immediately — the entry leaves
    /// the queue once the reply or the journaled
    /// `QueuedEntryCancelled` confirms it, so the row stays until then.
    fn cancel_queued_message(
        &mut self,
        entry_id: &str,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.client.active_session_id.clone() else {
            return;
        };
        let echo_id = format!("queued-user-{session_id}-{entry_id}");
        if restore {
            let staged = self
                .client
                .messages
                .iter()
                .find(|m| m.id == echo_id)
                .map(|m| m.content.clone())
                .unwrap_or_default();
            if !staged.is_empty() {
                self.composer
                    .update(cx, |input, cx| input.set_value(staged, window, cx));
            }
        }
        self.pending_queued_cancel = Some((session_id.clone(), entry_id.to_string()));
        if let Some(daemon) = &self.daemon {
            daemon.request(SessionCommand::CancelQueuedMessage {
                session_id,
                entry_id: entry_id.to_string(),
                work_dir: self.client.active_work_dir.clone(),
            });
        }
        self.client.session_status = Some("Removing queued message…".to_string());
        cx.notify();
    }

    /// Open the session's linked GitHub issue: jump to the Issues tab in
    /// the session's project scope and load the detail overlay.
    fn open_linked_issue(&mut self, number: u64, cx: &mut Context<Self>) {
        let Some(active) = &self.active else {
            return;
        };
        let Some(work_dir) = self
            .client
            .projects
            .iter()
            .find(|project| project.sessions.iter().any(|s| s.id == active.id))
            .map(|project| project.work_dir.clone())
        else {
            return;
        };
        self.select_project(work_dir, cx);
        self.select_tab(Tab::Issues, cx);
        self.issue_detail_pending = Some(number);
        self.send_github(GitHubOperation::InspectIssue { number });
        cx.notify();
    }

    fn answer_permission(&mut self, decision: PermissionDecision, cx: &mut Context<Self>) {
        let Some(active) = &mut self.active else {
            return;
        };
        let Some(request) = self.client.pending_permissions.get(&active.id).cloned() else {
            return;
        };
        // Commands queued while the socket is down are drained without
        // delivery, so keep the prompt unless a live socket can take it.
        let connected = self
            .daemon
            .as_ref()
            .is_some_and(|daemon| daemon.is_connected());
        if !connected {
            self.client.session_status = Some("Not connected — answer was not sent".to_string());
            cx.notify();
            return;
        }
        if let Some(daemon) = &self.daemon {
            daemon.request(SessionCommand::AnswerPermission {
                session_id: active.id.clone(),
                request_id: request.id,
                decision,
            });
        }
        cx.notify();
    }

    fn toggle_question_option(&mut self, key: &str, option: &str, cx: &mut Context<Self>) {
        let Some(active) = &mut self.active else {
            return;
        };
        let selected = active.answers.entry(key.to_string()).or_default();
        if let Some(position) = selected.iter().position(|picked| picked == option) {
            selected.remove(position);
        } else {
            selected.push(option.to_string());
        }
        cx.notify();
    }

    fn answer_question(&mut self, dismiss: bool, cx: &mut Context<Self>) {
        let Some(active) = &mut self.active else {
            return;
        };
        let Some(request) = self.client.pending_questions.get(&active.id).cloned() else {
            return;
        };
        let connected = self
            .daemon
            .as_ref()
            .is_some_and(|daemon| daemon.is_connected());
        if !connected {
            self.client.session_status = Some("Not connected — answer was not sent".to_string());
            cx.notify();
            return;
        }
        let answer = if dismiss {
            QuestionAnswer::dismissed(&request.id)
        } else {
            QuestionAnswer {
                request_id: request.id.clone(),
                dismissed: false,
                answers: request
                    .questions
                    .iter()
                    .map(|item| {
                        let key = question_key(&request.id, &item.id);
                        QuestionItemAnswer {
                            question_id: item.id.clone(),
                            selected: active.answers.get(&key).cloned().unwrap_or_default(),
                            custom_text: self
                                .question_inputs
                                .get(&key)
                                .map(|(input, _)| input.read(cx).value().trim().to_string())
                                .filter(|text| !text.is_empty()),
                        }
                    })
                    .collect(),
            }
        };
        // Submit stays disabled until something is answered, but never resolve
        // a totally unanswered card through any path — an empty answer is
        // indistinguishable from a real one downstream.
        if !dismiss
            && answer.answers.iter().all(|item| {
                item.selected.is_empty()
                    && item
                        .custom_text
                        .as_deref()
                        .is_none_or(|text| text.is_empty())
            })
        {
            return;
        }
        if let Some(daemon) = &self.daemon {
            daemon.request(SessionCommand::AnswerQuestion {
                session_id: active.id.clone(),
                answer,
            });
        }
        cx.notify();
    }

    fn cancel_run(&mut self, cx: &mut Context<Self>) {
        let Some(active) = &self.active else { return };
        if let Some(daemon) = &self.daemon {
            daemon.send(SessionCommand::CancelRun {
                session_id: active.id.clone(),
            });
        }
        cx.notify();
    }

    /// Archive the open session — the daemon applies its worktree/dirtiness
    /// guards and replies `SessionRemoved`, which navigates back to the
    /// session list. `delete_worktree` stays off: mobile has no worktree
    /// picker, so never drop a checkout from here.
    fn delete_active_session(&mut self, cx: &mut Context<Self>) {
        let Some(active) = &mut self.active else {
            return;
        };
        let connected = self
            .daemon
            .as_ref()
            .is_some_and(|daemon| daemon.is_connected());
        if !connected {
            self.client.session_status = Some("Not connected".to_string());
            cx.notify();
            return;
        }
        if let Some(daemon) = &self.daemon {
            daemon.send(SessionCommand::DeleteSession {
                session_id: active.id.clone(),
                session_file: active.session_file.clone(),
                delete_worktree: false,
            });
        }
        active.confirm_delete = false;
        self.client.session_status = Some("Archiving session…".to_string());
        cx.notify();
    }
}

impl MobileApp {
    /// Colored link-state dot for the sessions header.
    fn link_dot(&self, cx: &Context<Self>) -> impl IntoElement {
        let color = match self.link_state.as_str() {
            "Live" => cx.theme().success,
            "Connecting…" | "Reconnecting…" => cx.theme().warning,
            "Failed" => cx.theme().danger,
            _ => cx.theme().muted_foreground,
        };
        div()
            .flex_none()
            .size_2()
            .rounded_full()
            .bg(color)
            .mt(px(2.))
    }

    fn render_connect(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut saved = div().flex_none().flex().flex_col().gap_3();
        let devices = self.saved_devices();
        if !devices.is_empty() {
            saved = saved.child(div().text_sm().font_bold().child("Saved desktops"));
        }
        for device in devices {
            let id = device.id.clone();
            let remove_id = device.id.clone();
            let confirming = self.remove_device_confirmation.as_deref() == Some(&id);
            let selected = self.selected_device_id() == Some(&id);
            let current = self.current_device_id() == Some(&id);
            let status = if current { self.link_state.as_str() } else if selected {
                "Auto-connect on launch"
            } else { "Saved" };
            let mut row = div().flex().flex_col().gap_2().p_3().rounded_lg()
                .border_1().border_color(if selected { cx.theme().primary } else { cx.theme().border })
                .child(div().font_bold().child(device.name))
                .child(div().text_sm().text_color(cx.theme().muted_foreground)
                    .child(format!("{}:{} · {status}", device.host, device.port)))
                .child(div().flex().gap_2()
                    .child(Button::new(SharedString::from(format!("connect-{id}")))
                        .h_11().flex_1().label("Connect")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.remove_device_confirmation = None;
                            this.connect_saved_device(&id, window, cx);
                        })))
                    .child(Button::new(SharedString::from(format!("remove-{remove_id}")))
                        .ghost().h_11().label("Remove…")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_device_confirmation = Some(remove_id.clone());
                            cx.notify();
                        }))));
            if confirming {
                let id = device.id;
                row = row.child(div().text_sm().text_color(cx.theme().muted_foreground)
                    .child("Forget this desktop on this phone? Reconnecting will require its pairing link. To revoke access, also remove this phone on the desktop."))
                    .child(div().flex().gap_2()
                        .child(Button::new("cancel-remove-desktop").h_11().flex_1().label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remove_device_confirmation = None;
                                cx.notify();
                            })))
                        .child(Button::new("confirm-remove-desktop").danger().h_11().flex_1().label("Forget desktop")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Err(error) = this.remove_saved_device(&id, window, cx) {
                                    this.connect_error = Some(error);
                                }
                                this.remove_device_confirmation = None;
                                cx.notify();
                            }))));
            }
            saved = saved.child(row);
        }
        let mut brand = div().flex().items_center().gap_3();
        if let Some(logo) = threadlane_ui_theme::bundled_icon("icons/threadlane.svg") {
            brand = brand.child(logo.large().text_color(cx.theme().foreground));
        }
        let header = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .pt_4()
            .child(brand.child(div().text_xl().font_bold().child("Threadlane")))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Your selected desktop reconnects automatically. Keep the desktop app \
                         open and both devices on the same LAN or connected to Tailscale.",
                    ),
            );
        div()
            .id("pairing")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p_4()
            .gap_4()
            .child(header)
            .when(self.daemon.is_some(), |this| {
                this.child(Button::new("back-to-sessions").h_11().label("Back to sessions")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.screen = Screen::Main;
                        cx.notify();
                    })))
            })
            .child(saved)
            .when_some(self.connect_error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(div().flex_none().flex().flex_col().gap_2()
                .child(div().font_bold().child("Add a desktop"))
                .child(div().text_sm().text_color(cx.theme().muted_foreground)
                    .child("On your desktop, open Share with mobile → Add device. Scan the QR with your phone’s camera. You only need to do this once.")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Connection details — for Tailscale, use your desktop's Tailscale IPv4 address as Host. Keep the same port and token."))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Host"),
                            )
                            .child(Input::new(&self.host).large().aria_label("Desktop host")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Port"),
                            )
                            .child(Input::new(&self.port).large().aria_label("Desktop port")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Token"),
                            )
                            .child(Input::new(&self.token).large().aria_label("Pairing token")),
                    ),
            )
            .child(
                Button::new("connect")
                    .primary()
                    .h_11()
                    .label("Connect")
                    .w_full()
                    .disabled(self.link_state == "Connecting…")
                    .on_click(cx.listener(|this, _, window, cx| this.connect_now(window, cx))),
            )
    }

    fn render_sessions(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("open-sidebar")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::Menu.1))
                            .accessibility_label("Menu")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_open = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().font_bold().child("Chats"))
                                    .child(self.link_dot(cx)),
                            )
                            .when(self.link_state != "Live", |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(self.link_state.clone()),
                                )
                            }),
                    )
                    .child(
                        Button::new("new-session")
                            .ghost()
                            .size_11()
                            .icon(icon(kit_icons::SquarePen.1))
                            .tooltip("New chat")
                            .accessibility_label("New chat")
                            .disabled(
                                self.sending
                                    || self.client.projects.is_empty()
                                    || !self.daemon.as_ref().is_some_and(|d| d.is_connected()),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.begin_session(cx))),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .child(Input::new(&self.search).aria_label("Search chats").h_11()),
            )
            .when_some(self.connect_error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when(self.client.projects.is_empty(), |this| {
                this.child(
                    div()
                        .px_4()
                        .py_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Attach a project on desktop to start a chat."),
                )
            })
            .when(self.session_rows.is_empty() && !self.client.projects.is_empty(), |this| {
                this.child(div().px_4().py_4().text_sm().text_color(cx.theme().muted_foreground)
                    .child(if self.client.search_query.is_empty() { "Start a new chat to get going." } else { "No chats match your search." }))
            })
            .child(div().id("sessions").flex_1().min_h_0().w_full().child(
                threadlane_ui_kit::session_list(
                    self.sessions_list.clone(),
                    cx.processor(Self::render_session_row),
                ),
            ))
    }

    fn refresh_session_rows(&mut self) {
        let query = self.client.search_query.to_lowercase();
        self.session_rows = self
            .client
            .projects
            .iter()
            .flat_map(|project| &project.sessions)
            .filter(|session| query.is_empty() || session.title.to_lowercase().contains(&query))
            .cloned()
            .collect();
        self.session_rows.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        self.sessions_list.reset(self.session_rows.len());
    }

    fn render_session_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.session_rows.get(ix) {
            Some(session) => {
                let info = session.clone();
                let needs_you = self.client.pending_permissions.contains_key(&session.id)
                    || self.client.pending_questions.contains_key(&session.id);
                let attention = if needs_you {
                    threadlane_protocol::daemon::SessionAttention::NeedsYou
                } else if matches!(session.health, SessionHealth::Working) {
                    threadlane_protocol::daemon::SessionAttention::Working
                } else {
                    threadlane_protocol::daemon::SessionAttention::Idle
                };
                let identity = threadlane_ui_kit::session_identity(session);
                div()
                    .px_3()
                    .child(
                        threadlane_ui_kit::session_card(&session.id, false, cx)
                            .aria_label(format!("{}, {}", identity.title, attention.label()))
                            .child(
                                Button::new(format!("session-{}", session.id))
                                    .ghost()
                                    .h_auto()
                                    .min_h_16()
                                    .py_3()
                                    .w_full()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .items_start()
                                            .gap_1()
                                            .w_full()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .w_full()
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .truncate()
                                                            .child(identity.title),
                                                    )
                                                    .children(
                                                        threadlane_ui_kit::session_attention(
                                                            &session.id,
                                                            attention,
                                                            cx,
                                                        ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .truncate()
                                                    .child(format!(
                                                        "{} · {}{}",
                                                        self.client.projects.iter().find(|project| project.work_dir == session.work_dir).map(|project| project.name.as_str()).unwrap_or("Project"),
                                                        session
                                                            .git_branch
                                                            .as_deref()
                                                            .unwrap_or("Local"),
                                                        if session.is_worktree {
                                                            " · worktree"
                                                        } else {
                                                            ""
                                                        }
                                                    )),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_session(&info, window, cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn render_session(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let uncertain_prompt = self.client.active_session_id.as_ref()
            .is_some_and(|id| self.uncertain_prompts.contains(id));
        let Some(active) = &self.active else {
            return div().size_full().bg(cx.theme().background);
        };
        let title = active.title.clone();
        let working = self.client.is_generating;
        let status = self.client.session_status.clone();
        let permission = self.client.pending_permissions.get(&active.id).cloned();
        let question = self.client.pending_questions.get(&active.id).cloned();
        let confirm_delete = active.confirm_delete;
        let composer_empty = self.composer.read(cx).value().trim().is_empty();
        let session_meta = {
            let mut parts: Vec<String> = Vec::new();
            if let Some(branch) = &active.git_branch {
                if !branch.is_empty() {
                    parts.push(branch.clone());
                }
            }
            if active.is_worktree {
                parts.push("worktree".to_string());
            }
            parts.join(" · ")
        };
        let linked_issue = active.github_issue.clone();
        let connected = self.daemon.as_ref().is_some_and(|d| d.is_connected());
        let send_disabled = composer_empty
            || self.sending
            || self.composer_pending()
            || self.selected_model.is_none()
            || uncertain_prompt
            || !connected;
        // ACP agents can queue follow-ups but cannot take a mid-turn steer.
        let acp = self.acp_selected();
        let steer_tip = if acp {
            "This agent does not support live steering. Use Queue for the next turn."
        } else {
            "Interrupt the current turn"
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("back")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::ArrowLeft.1))
                            .accessibility_label("Back to chats")
                            .tooltip("Back to chats")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let key = (
                                    this.client.active_work_dir.clone(),
                                    this.client.active_session_id.clone(),
                                );
                                this.client.composer_drafts.insert(
                                    key,
                                    threadlane_client::ComposerDraft {
                                        text: this.composer.read(cx).value().to_string(),
                                        images: Vec::new(),
                                    },
                                );
                                this.active = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_bold().truncate().child(title))
                            .when(!session_meta.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(session_meta),
                                )
                            })
                            .when_some(status, |this, status| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(status),
                                )
                            }),
                    )
                    .when_some(linked_issue, |this, issue| {
                        let number = issue.number;
                        this.child(
                            Button::new(format!("linked-issue-{number}"))
                                .ghost()
                                .small()
                                .h_10()
                                .icon(icon(kit_icons::CircleDot.1))
                                .label(format!("#{number}"))
                                .tooltip("Open linked issue")
                                .accessibility_label(format!(
                                    "Open linked issue number {number}"
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_linked_issue(number, cx)
                                })),
                        )
                    })
                    .when(working, |this| {
                        this.child(
                            Button::new("cancel-run")
                                .danger()
                                .h_11()
                                .label("Stop")
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_run(cx))),
                        )
                    })
                    .child(
                        Button::new("session-options")
                            .ghost()
                            .size_11()
                            .icon(icon(kit_icons::Ellipsis.1))
                            .accessibility_label("Session options")
                            .tooltip("Session options")
                            .dropdown_menu_with_anchor(Anchor::TopRight, {
                                let entity = cx.entity();
                                let status = self.client.session_status.clone();
                                move |menu, _, _| {
                                    let menu = menu.when_some(status.clone(), |menu, status| {
                                        menu.item(PopupMenuItem::new("Session status…").on_click(move |_, window, cx| {
                                            let status = status.clone();
                                            window.open_dialog(cx, move |dialog, _, _| {
                                                dialog.title("Session status")
                                                    .child(div().text_sm().child(status.clone()))
                                                    .footer(Button::new("status-done").ghost().h_11().label("Done")
                                                        .on_click(|_, window, cx| window.close_dialog(cx)))
                                            });
                                        }))
                                    });
                                    let refresh = entity.clone();
                                    let new = entity.clone();
                                    let archive = entity.clone();
                                    menu.item(PopupMenuItem::new("Refresh chat").on_click(
                                        move |_, _, cx| {
                                            refresh.update(cx, |this, _| {
                                                if let (Some(active), Some(daemon)) =
                                                    (&this.active, &this.daemon)
                                                {
                                                    daemon.send(
                                                        SessionCommand::GetSessionSnapshot {
                                                            session_id: active.id.clone(),
                                                        },
                                                    );
                                                }
                                            });
                                        },
                                    ))
                                    .item(PopupMenuItem::new("New chat in project").on_click(
                                        move |_, _, cx| {
                                            new.update(cx, |this, cx| this.begin_session(cx));
                                        },
                                    ))
                                    .item(
                                        PopupMenuItem::new("Archive chat…").on_click(
                                            move |_, _, cx| {
                                                archive.update(cx, |this, cx| {
                                                    if let Some(active) = &mut this.active {
                                                        active.confirm_delete = true;
                                                    }
                                                    cx.notify();
                                                });
                                            },
                                        ),
                                    )
                                }
                            }),
                    ),
            )
            .when(confirm_delete, |this| {
                this.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted.opacity(0.3))
                        .child(div().flex_1().text_xs().child("Archive this session?"))
                        .child(
                            Button::new("confirm-delete")
                                .danger()
                                .small()
                                .label("Archive")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.delete_active_session(cx)),
                                ),
                        )
                        .child(
                            Button::new("cancel-delete")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(active) = &mut this.active {
                                        active.confirm_delete = false;
                                    }
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(div().id("transcript").flex_1().min_h_0().w_full().child(
                threadlane_ui_kit::transcript_list(
                    &self.active.as_ref().unwrap().transcript,
                    cx.processor(Self::render_transcript_row),
                ),
            ))
            .when_some(permission, |this, permission| {
                this.child(self.render_permission(&permission, cx))
            })
            .when_some(question, |this, question| {
                this.child(self.render_question(&question, window, cx))
            })
            .when_some(self.render_queued_strip(cx), |this, strip| {
                this.child(strip)
            })
            .child(
                div().flex_none().w_full().px_3().mb_3().child(
                    threadlane_ui_kit::composer_surface(
                        self.composer.read(cx).focus_handle(cx).is_focused(window), cx,
                    )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(threadlane_ui_kit::composer_input(&self.composer)),
                        )
                        .when(uncertain_prompt, |this| this.child(
                            div().flex().flex_col().gap_1()
                                .child("Submission is unconfirmed. Check the chat before sending again; the original prompt may still run.")
                                .child(Button::new("acknowledge-uncertain-prompt").ghost().label("I checked — allow another send")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(active) = &this.active { this.uncertain_prompts.remove(&active.id); }
                                        cx.notify();
                                    })))
                        ))
                        .child(
                            div().flex().items_center().gap_2()
                                .child(div().flex_1().min_w_0().child(self.composer_project(cx)))
                                .child(
                                    Button::new("configuration")
                                        .ghost()
                                        .h_11()
                                        .flex_1()
                                        .min_w_0()
                                        .icon(icon(kit_icons::Settings2.1))
                                        .child(div().min_w_0().truncate().text_sm().child(self.composer_model_label()))
                                        .tooltip("Configuration…")
                                        .accessibility_label(format!("Configuration: {}, {}, {}", self.composer_model_label(), self.effort.map(|effort| effort.label()).unwrap_or("Default effort"), self.mode.label()))
                                        .on_click(cx.listener(|this, _, window, cx| this.open_configuration(window, cx))),
                                )
                                .when(working, |this| {
                                    this.child(
                                        Button::new("steer")
                                            .ghost()
                                            .h_11()
                                            .icon(icon(kit_icons::Zap.1))
                                            .label("Steer")
                                            .tooltip(steer_tip)
                                            .accessibility_label(steer_tip)
                                            .disabled(send_disabled || acp)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.steer_composer(window, cx)
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("send")
                                        .primary()
                                        .size_11()
                                        .icon(icon(kit_icons::SendHorizontal.1))
                                        .accessibility_label(if working {
                                            "Queue for next turn"
                                        } else {
                                            "Send message"
                                        })
                                        .disabled(send_disabled)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit_composer(window, cx)
                                        })),
                                ),
                        ),
                ),
            )
    }

    fn render_transcript_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use threadlane_ui_kit::transcript::TranscriptRow;
        let Some(active) = &self.active else {
            return div().into_any_element();
        };
        let messages = active.transcript.messages.clone();
        match active.transcript.rows.get(ix).cloned() {
            Some(TranscriptRow::Message(index)) => messages
                .get(index)
                .map(|m| self.render_message(m, window, ix, cx))
                .unwrap_or_else(|| div().into_any_element()),
            Some(TranscriptRow::Activities(range)) => div()
                .flex()
                .flex_col()
                .children(messages[range].iter().map(|m| self.render_message(m, window, ix, cx)))
                .into_any_element(),
            Some(TranscriptRow::Working) => div()
                .px_5()
                .child(
                    Marker::new()
                        .id("session-working")
                        .role(Role::Status)
                        .loading(true)
                        .with_loading_style(MarkerLoadingStyle::Shimmer)
                        .content(MarkerContent::new().text("Working…")),
                )
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn render_message(
        &mut self,
        message: &ChatMessageInfo,
        window: &mut Window,
        row_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_user = message.role == MessageRole::User;
        let namespace =
            SharedString::from(self.client.active_session_id.clone().unwrap_or_default());
        let state = threadlane_ui_kit::markdown::markdown_state(
            &mut self.markdown_states,
            namespace,
            message.id.clone(),
            &message.content,
            cx,
        );
        let reasoning = message
            .reasoning_content
            .as_ref()
            .filter(|text| !text.trim().is_empty())
            .map(|reasoning| {
                let motion = threadlane_ui_kit::DisclosureMotion::new(
                    SharedString::from(format!("reasoning-body-{}", message.id)),
                    message.reasoning_expanded,
                    window,
                    cx,
                );
                if let Some(active) = &self.active {
                    motion.remeasure_list_row(&active.transcript.list, row_index, window);
                }
                let detail = motion.is_visible().then(|| {
                    if message.streaming {
                        threadlane_ui_kit::reasoning_detail(cx)
                            .child(reasoning.clone())
                            .into_any_element()
                    } else {
                        let namespace = SharedString::from(
                            self.client.active_session_id.clone().unwrap_or_default(),
                        );
                        let markdown = threadlane_ui_kit::markdown::markdown_state(
                            &mut self.markdown_states,
                            namespace,
                            format!("reasoning-{}", message.id),
                            reasoning,
                            cx,
                        );
                        threadlane_ui_kit::reasoning_detail(cx)
                            .child(threadlane_ui_kit::markdown::markdown_view(
                                &markdown,
                                |_, _| {},
                            ))
                            .into_any_element()
                    }
                }).map(|body| motion.content(body));
                let owner = cx.entity().downgrade();
                let id = message.id.clone();
                threadlane_ui_kit::reasoning_card(
                    message,
                    detail,
                    true,
                    move |_, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            if let Some(message) =
                                this.client.messages_mut().iter_mut().find(|m| m.id == id)
                            {
                                message.reasoning_expanded = !message.reasoning_expanded;
                            }
                            if let Some(active) = &mut this.active {
                                active.transcript.list.pause_following_tail();
                                active.transcript.sync(
                                    this.client.messages.clone(),
                                    this.client.is_generating,
                                    false,
                                    true,
                                );
                            }
                            cx.notify();
                        });
                    },
                    cx,
                )
            });
        let tools = message
            .tool_activities
            .iter()
            .map(|tool| {
                let motion = threadlane_ui_kit::DisclosureMotion::new(
                    SharedString::from(format!("tool-body-{}", tool.id)),
                    tool.is_expanded,
                    window,
                    cx,
                );
                if let Some(active) = &self.active {
                    motion.remeasure_list_row(&active.transcript.list, row_index, window);
                }
                let detail = motion.is_visible().then(|| {
                    threadlane_ui_kit::tool_detail(cx)
                        .child(tool.detail.clone())
                        .into_any_element()
                }).map(|body| motion.content(body));
                let owner = cx.entity().downgrade();
                let id = tool.id.clone();
                threadlane_ui_kit::tool_activity(
                    tool,
                    !tool.detail.trim().is_empty(),
                    detail,
                    true,
                    move |_, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            if let Some(tool) = this
                                .client
                                .messages_mut()
                                .iter_mut()
                                .flat_map(|m| &mut m.tool_activities)
                                .find(|tool| tool.id == id)
                            {
                                tool.is_expanded = !tool.is_expanded;
                            }
                            if let Some(active) = &mut this.active {
                                active.transcript.list.pause_following_tail();
                                active.transcript.sync(
                                    this.client.messages.clone(),
                                    this.client.is_generating,
                                    false,
                                    true,
                                );
                            }
                            cx.notify();
                        });
                    },
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let body = div()
            .text_sm()
            .line_height(relative(1.5))
            .children(reasoning)
            .when(!message.content.is_empty(), |el| {
                el.child(threadlane_ui_kit::markdown::markdown_view(
                    &state,
                    |_, _| {},
                ))
            })
            .when(message.streaming && message.content.is_empty(), |this| {
                this.text_color(cx.theme().muted_foreground).child("…")
            })
            .children(tools);
        if is_user {
            let session_id = self
                .client
                .active_session_id
                .clone()
                .unwrap_or_default();
            let is_steered = message
                .id
                .starts_with(&format!("steered-user-{session_id}-"));
            let mut row = threadlane_ui_kit::message_row(MessageRole::User)
                .child(threadlane_ui_kit::user_message_bubble(cx).child(body));
            if is_steered {
                row = row.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .px_3()
                        .pb_1()
                        .child(Tag::new().small().child("Steered")),
                );
            }
            row.into_any_element()
        } else {
            threadlane_ui_kit::message_row(message.role.clone())
                .child(body)
                .into_any_element()
        }
    }

    /// Pending follow-ups are filtered out of the transcript rows by the
    /// shared builder (`is_queued_message`), so they get the desktop's
    /// own strip above the composer — echo rows without an entry id show
    /// no controls until `FollowUpQueued` binds them.
    fn render_queued_strip(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session_id = self.client.active_session_id.clone()?;
        let generating = self.client.is_generating;
        let queued: Vec<(String, String)> = self
            .client
            .messages
            .iter()
            .filter(|m| threadlane_ui_kit::transcript::is_queued_message(m, generating))
            .map(|m| (m.id.clone(), m.content.clone()))
            .collect();
        if queued.is_empty() {
            return None;
        }
        let acp = self.acp_selected();
        let steer_tip = if acp {
            "This agent does not support live steering. Use Queue for the next turn."
        } else {
            "Steer the current turn with this message"
        };
        let prefix = format!("queued-user-{session_id}-");
        Some(
            div()
                .flex_none()
                .w_full()
                .px_3()
                .mb_2()
                .child(
                    div()
                        .w_full()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().sidebar_border)
                        .bg(cx.theme().sidebar)
                        .child(
                            div()
                                .w_full()
                                .px_3()
                                .py_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "Queued messages ({})",
                                    queued.len()
                                )),
                        )
                        .children(queued.into_iter().map(|(message_id, text)| {
                            let entry_id = message_id
                                .strip_prefix(&prefix)
                                .map(|entry| entry.to_string());
                            let cancel_pending = entry_id.as_ref().is_some_and(|entry| {
                                self.pending_queued_cancel.as_ref().is_some_and(|(sid, eid)| {
                                    sid == &session_id && eid == entry
                                })
                            });
                            let mut row = div()
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .border_t_1()
                                .border_color(cx.theme().sidebar_border)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .line_clamp(2)
                                        .child(text),
                                );
                            if let Some(entry_id) = entry_id {
                                if cancel_pending {
                                    row = row.child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Removing…"),
                                    );
                                } else {
                                    let steer_id = entry_id.clone();
                                    let edit_id = entry_id.clone();
                                    let remove_id = entry_id.clone();
                                    row = row
                                        .child(
                                            Button::new(format!("queued-steer-{entry_id}"))
                                                .ghost()
                                                .small()
                                                .h_9()
                                                .icon(icon(kit_icons::Zap.1))
                                                .label("Steer")
                                                .tooltip(steer_tip)
                                                .accessibility_label(steer_tip)
                                                .disabled(acp)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.steer_queued(&steer_id, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new(format!("queued-edit-{entry_id}"))
                                                .ghost()
                                                .small()
                                                .h_9()
                                                .label("Edit")
                                                .tooltip("Restore to composer")
                                                .accessibility_label(
                                                    "Restore queued message to composer",
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.cancel_queued_message(
                                                            &edit_id, true, window, cx,
                                                        )
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new(format!("queued-remove-{entry_id}"))
                                                .ghost()
                                                .small()
                                                .h_9()
                                                .icon(icon(kit_icons::X.1))
                                                .label("Remove")
                                                .tooltip("Discard queued message")
                                                .accessibility_label("Discard queued message")
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.cancel_queued_message(
                                                            &remove_id, false, window, cx,
                                                        )
                                                    },
                                                )),
                                        );
                                }
                            }
                            row
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_permission(
        &self,
        request: &PermissionRequest,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let connected = self.daemon.as_ref().is_some_and(|d| d.is_connected());
        threadlane_ui_kit::permission_card(
            request,
            true,
            connected,
            None,
            move |request_id, decision, _, cx| {
                let _ = owner.update(cx, |this, cx| {
                    if this
                        .client
                        .active_session_id
                        .as_ref()
                        .and_then(|id| this.client.pending_permissions.get(id))
                        .is_some_and(|r| r.id == request_id)
                    {
                        this.answer_permission(decision, cx);
                    }
                });
            },
            cx,
        )
    }

    /// The front question request: option toggles per item, one custom-answer
    /// input per `allow_custom` item, then Submit/Dismiss.
    fn render_question(
        &mut self,
        request: &QuestionRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(active) = &self.active else {
            return div().into_any_element();
        };
        let answers = active.answers.clone();
        // Lazily create one custom-text input per allow_custom question so the
        // entity (and focus) survives re-renders.
        for item in request.questions.iter().filter(|item| item.allow_custom) {
            let key = question_key(&request.id, &item.id);
            if !self.question_inputs.contains_key(&key) {
                let input = cx
                    .new(|cx| InputState::new(window, cx).placeholder("Custom answer (optional)…"));
                let subscription =
                    cx.subscribe_in(&input, window, |_, _, event, _, _| match event {
                        InputEvent::Focus => gpui_mobile::show_keyboard(),
                        InputEvent::Blur => gpui_mobile::hide_keyboard(),
                        _ => {}
                    });
                self.question_inputs.insert(key, (input, subscription));
            }
        }
        // Drop state for superseded requests so a new question starts clean.
        let prefix = format!("{}\0", request.id);
        self.question_inputs
            .retain(|key, _| key.starts_with(&prefix));
        let mut items = Vec::new();
        for item in &request.questions {
            let key = question_key(&request.id, &item.id);
            let owner = cx.entity().downgrade();
            let option_key = key.clone();
            items.push(threadlane_ui_kit::question_item(
                &request.id,
                item,
                answers.get(&key).map(Vec::as_slice).unwrap_or_default(),
                self.question_inputs.get(&key).map(|(input, _)| input),
                true,
                move |value, _, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        this.toggle_question_option(&option_key, value, cx)
                    });
                },
                cx,
            ));
        }
        // Sending with zero selections and zero custom text resolves an empty
        // answer (indistinguishable from a real one downstream), so Submit
        // stays disabled until something is answered.
        let has_answer = request.questions.iter().any(|item| {
            let key = question_key(&request.id, &item.id);
            let selected = answers.get(&key).is_some_and(|picked| !picked.is_empty());
            let custom = self
                .question_inputs
                .get(&key)
                .is_some_and(|(input, _)| !input.read(cx).value().trim().is_empty());
            selected || custom
        });
        let remaining = self
            .client
            .queued_questions
            .get(&active.id)
            .map_or(0, Vec::len);
        threadlane_ui_kit::question_surface(cx)
            .flex_none()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        icon(kit_icons::CircleQuestionMark.1)
                            .xsmall()
                            .text_color(cx.theme().accent),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_bold()
                            .child(if request.questions.len() > 1 {
                                format!("The agent has {} questions", request.questions.len())
                            } else {
                                "The agent has a question".to_string()
                            }),
                    )
                    .when(remaining > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("+{remaining} more")),
                        )
                    }),
            )
            .children(items)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("question-submit")
                            .primary()
                            .small()
                            .label("Submit")
                            .disabled(!has_answer)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.answer_question(false, cx)),
                            ),
                    )
                    .child(
                        Button::new("question-dismiss")
                            .ghost()
                            .small()
                            .label("Dismiss")
                            .on_click(cx.listener(|this, _, _, cx| this.answer_question(true, cx))),
                    ),
            )
            .into_any_element()
    }

    /// Route a `GitRequest` at the sidebar-selected project, if one is set.
    fn send_git(&self, operation: GitOperation) {
        if let (Some(work_dir), Some(daemon)) =
            (self.client.sidebar_project_filter.clone(), &self.daemon)
        {
            daemon.request(SessionCommand::GitRequest { work_dir, operation });
        }
    }

    /// Route a `GitHubRequest` at the sidebar-selected project — the
    /// daemon shells out to `gh` against that checkout's forge remote.
    fn send_github(&self, operation: GitHubOperation) {
        if let (Some(work_dir), Some(daemon)) =
            (self.client.sidebar_project_filter.clone(), &self.daemon)
        {
            daemon.request(SessionCommand::GitHubRequest { work_dir, operation });
        }
    }

    fn refresh_issues(&self) {
        self.send_github(GitHubOperation::ListIssues {
            state: self.issue_filter,
            query: None,
            limit: 30,
        });
    }

    fn refresh_prs(&self) {
        self.send_github(GitHubOperation::ListPullRequests {
            state: self.pr_filter,
            query: None,
            limit: 30,
        });
    }

    /// The automation store is daemon-global (not per project), so this
    /// needs no work_dir.
    fn send_automation(&self, command: AutomationCommand) {
        if let Some(daemon) = &self.daemon {
            daemon.request(SessionCommand::AutomationRequest { command });
        }
    }

    fn refresh_automations(&self) {
        self.send_automation(AutomationCommand::GetSnapshot);
    }

    /// Open the editor: `Some(definition)` pre-fills for editing, `None`
    /// starts a fresh draft scoped to the selected project.
    fn open_auto_form(&mut self, definition: Option<Definition>, window: &mut Window, cx: &mut Context<Self>) {
        let draft = definition.unwrap_or_else(|| Definition {
            id: threadlane_protocol::automation::new_id(),
            revision: 0,
            name: String::new(),
            prompt: String::new(),
            project: self
                .client
                .sidebar_project_filter
                .clone()
                .or_else(|| self.client.projects.first().map(|p| p.work_dir.clone()))
                .unwrap_or_default(),
            model: self.selected_model.clone().unwrap_or_default(),
            effort: self
                .effort
                .map(|effort| effort.label().to_string())
                .unwrap_or_else(|| "medium".into()),
            worktree: true,
            schedule: Schedule::Interval { minutes: 60 },
            enabled: true,
            notify_all: false,
            anchor: threadlane_protocol::automation::now(),
            next_at: None,
            failures: 0,
            paused_reason: None,
        });
        self.auto_schedule = match &draft.schedule {
            Schedule::Manual => AutoSchedule::Manual,
            Schedule::Interval { .. } => AutoSchedule::Interval,
            // A calendar the form can't express stays selected via
            // `Existing`; picking a preset is the only way it changes.
            Schedule::Calendar { .. } => AutoSchedule::Existing,
        };
        self.auto_worktree = draft.worktree;
        self.auto_enabled = draft.enabled;
        for (entity, value) in [
            (&self.auto_name, draft.name.clone()),
            (&self.auto_model, draft.model.clone()),
            (&self.auto_effort, draft.effort.clone()),
            (
                &self.auto_minutes,
                match &draft.schedule {
                    Schedule::Interval { minutes } => minutes.to_string(),
                    _ => "60".into(),
                },
            ),
        ] {
            entity.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.auto_prompt.update(cx, |input, cx| {
            input.set_value(draft.prompt.clone(), window, cx)
        });
        self.auto_editing = Some(draft);
        cx.notify();
    }

    /// Save the editor's draft: read inputs, rebuild the Definition,
    /// validate locally, and send `Save`. The panel closes when the
    /// projection reply confirms it landed.
    fn save_auto_form(&mut self, cx: &mut Context<Self>) {
        let Some(mut draft) = self.auto_editing.clone() else {
            return;
        };
        draft.name = self.auto_name.read(cx).value().trim().to_owned();
        draft.model = self.auto_model.read(cx).value().trim().to_owned();
        draft.effort = self.auto_effort.read(cx).value().trim().to_owned();
        draft.prompt = self.auto_prompt.read(cx).value().trim().to_owned();
        draft.worktree = self.auto_worktree;
        draft.enabled = self.auto_enabled;
        draft.schedule = match self.auto_schedule {
            // `auto_editing` still carries the loaded calendar — keep it.
            AutoSchedule::Existing => draft.schedule.clone(),
            AutoSchedule::Manual => Schedule::Manual,
            AutoSchedule::Interval => Schedule::Interval {
                minutes: self
                    .auto_minutes
                    .read(cx)
                    .value()
                    .trim()
                    .parse()
                    .unwrap_or(0),
            },
            AutoSchedule::DailyUtc => Schedule::Calendar {
                hour: 9,
                minute: 0,
                days: (0..7).collect(),
                timezone: "UTC".into(),
            },
        };
        if draft.project.as_os_str().is_empty() {
            self.client.session_status = Some("Choose a project first".into());
            cx.notify();
            return;
        }
        if let Err(error) = draft.validate() {
            self.client.session_status = Some(error);
            cx.notify();
            return;
        }
        self.send_automation(AutomationCommand::Save {
            definition: draft,
        });
        cx.notify();
    }

    /// Stash both detail comment inputs under their target keys so a
    /// draft typed on one issue/PR cannot leak onto the next one opened.
    fn stash_comment_drafts(&mut self, cx: &mut Context<Self>) {
        let Some(work_dir) = self.client.sidebar_project_filter.clone() else {
            return;
        };
        if let Some(detail) = &self.issue_detail {
            let key = format!("{}|issue-{}", work_dir.display(), detail.summary.issue.number);
            let text = self.issue_comment.read(cx).value().to_string();
            if text.trim().is_empty() {
                self.comment_drafts.remove(&key);
            } else {
                self.comment_drafts.insert(key, text);
            }
        }
        if let Some(pr) = &self.pr_detail {
            let key = format!("{}|pr-{}", work_dir.display(), pr.number);
            let text = self.pr_comment.read(cx).value().to_string();
            if text.trim().is_empty() {
                self.comment_drafts.remove(&key);
            } else {
                self.comment_drafts.insert(key, text);
            }
        }
    }

    /// Load the stashed draft (or blank) for a target about to open.
    fn restore_comment_draft(
        &mut self,
        scope: &str,
        number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self
            .client
            .sidebar_project_filter
            .as_ref()
            .and_then(|work_dir| {
                self.comment_drafts
                    .remove(&format!("{}|{scope}-{number}", work_dir.display()))
            })
            .unwrap_or_default();
        let input = if scope == "issue" {
            &self.issue_comment
        } else {
            &self.pr_comment
        };
        input.update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// The sidebar's project pick is the scope for every panel — chats and
    /// Git both read `sidebar_project_filter`, so switching re-scopes the
    /// whole surface at once.
    fn select_project(&mut self, work_dir: std::path::PathBuf, cx: &mut Context<Self>) {
        self.client.sidebar_project_filter = Some(work_dir.clone());
        self.sidebar_open = false;
        self.git_diff = None;
        self.stash_drop_confirm = None;
        // An open chat belongs to its project: keep the composer draft
        // under its key, then detach it so Chats lands on the newly
        // scoped session list instead of still driving the old project.
        if self
            .client
            .active_work_dir
            .as_ref()
            .is_some_and(|dir| dir != &work_dir)
        {
            if self.active.is_some() {
                self.client.composer_drafts.insert(
                    (
                        self.client.active_work_dir.clone(),
                        self.client.active_session_id.clone(),
                    ),
                    threadlane_client::ComposerDraft {
                        text: self.composer.read(cx).value().to_string(),
                        images: Vec::new(),
                    },
                );
                self.active = None;
            }
            self.client.active_work_dir = None;
            self.client.active_session_id = None;
            self.client.messages = Default::default();
            self.client.is_generating = false;
        }
        self.stash_comment_drafts(cx);
        self.issues = None;
        self.issue_detail = None;
        self.issue_detail_pending = None;
        self.prs = None;
        self.pr_detail = None;
        self.pr_detail_pending = None;
        self.send_git(GitOperation::Inspect { sync_remote: false });
        if matches!(self.tab, Tab::Issues) {
            self.refresh_issues();
        } else if matches!(self.tab, Tab::Pulls) {
            self.refresh_prs();
        }
        self.refresh_session_rows();
        cx.notify();
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.sidebar_open = false;
        match tab {
            Tab::Git => {
                self.git_diff = None;
                self.send_git(GitOperation::Inspect { sync_remote: false });
            }
            Tab::Issues => {
                if self.issues.is_none() {
                    self.refresh_issues();
                }
            }
            Tab::Pulls => {
                if self.prs.is_none() {
                    self.refresh_prs();
                }
            }
            Tab::Automations => {
                self.refresh_automations();
            }
            Tab::Chats => {}
        }
        cx.notify();
    }

    /// File rows open a full-screen diff rather than a popover — a phone
    /// has no hover, and a code view wants the whole viewport.
    fn open_file_diff(
        &mut self,
        title: String,
        operation: GitOperation,
        cx: &mut Context<Self>,
    ) {
        self.git_diff_pending = Some(title);
        self.send_git(operation);
        cx.notify();
    }

    /// `stash-<i>`/`commit-<sha>` keys repeat across projects, so the
    /// expansion caches are namespaced by the sidebar pick.
    fn git_object_key(&self, key: &str) -> String {
        match &self.client.sidebar_project_filter {
            Some(dir) => format!("{}|{key}", dir.display()),
            None => key.to_string(),
        }
    }

    /// Expand or collapse a commit/stash object on the Git tab, fetching
    /// its file list the first time it opens.
    fn toggle_git_object(
        &mut self,
        key: String,
        operation: GitOperation,
        cx: &mut Context<Self>,
    ) {
        if self.git_expanded.contains(&key) {
            self.git_expanded.remove(&key);
        } else {
            self.git_expanded.insert(key.clone());
            if !self.git_object_files.contains_key(&key) {
                self.send_git(operation);
            }
        }
        cx.notify();
    }
}

impl MobileApp {
    /// The shell: tabbed content over a bottom tab bar, with the sidebar
    /// and the diff overlay stacked above both.
    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.tab {
            Tab::Chats => {
                if self.active.is_some() {
                    self.render_session(window, cx).into_any_element()
                } else {
                    self.render_sessions(cx).into_any_element()
                }
            }
            Tab::Git => self.render_git_panel(cx).into_any_element(),
            Tab::Issues => self.render_issues_panel(cx).into_any_element(),
            Tab::Pulls => self.render_pulls_panel(cx).into_any_element(),
            Tab::Automations => self.render_automations_panel(cx).into_any_element(),
        };
        div()
            .id("mobile-main")
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h_0().w_full().child(content))
                    // Chat owns the phone's work area; Back restores all destinations.
                    .when(self.tab != Tab::Chats || self.active.is_none(), |this| {
                        this.child(self.render_tab_bar(cx))
                    }),
            )
            .when(self.sidebar_open, |this| {
                this.child(self.render_sidebar(window, cx))
            })
            .when_some(self.issue_detail.clone(), |this, detail| {
                this.child(self.render_issue_detail(&detail, cx))
            })
            .when_some(self.pr_detail.clone(), |this, pr| {
                this.child(self.render_pr_detail(&pr, cx))
            })
            .when(self.new_issue_open, |this| {
                this.child(self.render_new_issue(cx))
            })
            .when(self.auto_editing.is_some(), |this| {
                this.child(self.render_auto_form(cx))
            })
            .when_some(self.git_diff.clone(), |this, (title, text)| {
                this.child(self.render_git_diff(&title, &text, cx))
            })
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = [
            (Tab::Chats, "Chats", kit_icons::MessagesSquare.1),
            (Tab::Git, "Git", kit_icons::GitBranch.1),
            (Tab::Issues, "Issues", kit_icons::CircleDot.1),
            (Tab::Pulls, "PRs", kit_icons::GitPullRequest.1),
            (Tab::Automations, "Automations", kit_icons::Zap.1),
        ];
        div()
            .flex_none()
            .w_full()
            .flex()
            .items_stretch()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .children(tabs.into_iter().map(|(tab, label, bytes)| {
                let color = if self.tab == tab {
                    cx.theme().primary
                } else {
                    cx.theme().muted_foreground
                };
                Button::new(format!("tab-{label}"))
                    .ghost()
                    .flex_1()
                    .h_12()
                    .accessibility_label(format!("{label} tab"))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .child(icon(bytes).small().text_color(color))
                            .child(div().text_xs().text_color(color).child(label)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select_tab(tab, cx)))
            }))
    }

    /// Slide-over drawer: the phone has no room for a persistent sidebar,
    /// so project picking and connection controls live behind the scrim.
    fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.viewport_size().width * 0.82;
        div()
            .id("mobile-sidebar")
            .absolute()
            .inset_0()
            .child(
                div()
                    .id("sidebar-scrim")
                    .absolute()
                    .inset_0()
                    .bg(threadlane_ui_theme::overlay_scrim())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = false;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("sidebar-panel")
                    .absolute()
                    .top_0()
                    .left_0()
                    .bottom_0()
                    .w(width)
                    .flex()
                    .flex_col()
                    .bg(cx.theme().sidebar)
                    .border_r_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(cx.theme().sidebar_border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(div().font_bold().child("Threadlane"))
                                            .child(self.link_dot(cx)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(self.link_state.clone()),
                                    ),
                            )
                            .child(
                                Button::new("close-sidebar")
                                    .ghost()
                                    .icon(icon(kit_icons::X.1))
                                    .accessibility_label("Close menu")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div().flex_none().px_4().pt_3().pb_1().child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Projects"),
                        ),
                    )
                    .child(
                        div()
                            .id("sidebar-projects")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .children(self.client.projects.iter().map(|project| {
                                self.render_sidebar_project(project, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .border_t_1()
                            .border_color(cx.theme().sidebar_border)
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                Button::new("sidebar-disconnect")
                                    .ghost()
                                    .w_full()
                                    .h_11()
                                    .icon(icon(kit_icons::Unplug.1))
                                    .label("Disconnect")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_open = false;
                                        this.disconnect(cx);
                                    })),
                            )
                            .child(
                                    Button::new("sidebar-shared-desktops")
                                        .ghost()
                                        .w_full()
                                        .h_11()
                                        .label("Shared desktops")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sidebar_open = false;
                                            this.screen = Screen::Connect;
                                            cx.notify();
                                        })),
                            ),
                    ),
            )
    }

    fn render_sidebar_project(
        &self,
        project: &threadlane_protocol::daemon::ProjectInfo,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let work_dir = project.work_dir.clone();
        let selected = self.client.sidebar_project_filter.as_ref() == Some(&work_dir);
        let accent = if selected {
            cx.theme().sidebar_accent
        } else {
            cx.theme().muted_foreground
        };
        let status = self
            .project_git
            .get(&work_dir)
            .map(|git| {
                format!(
                    "{} · {}{}",
                    git.branch.as_deref().unwrap_or("Detached"),
                    if git.has_changes { "Modified" } else { "Clean" },
                    if git.ahead > 0 || git.behind > 0 {
                        format!(" · ↑{} ↓{}", git.ahead, git.behind)
                    } else {
                        String::new()
                    }
                )
            })
            .unwrap_or_else(|| "Git status unavailable".into());
        div()
            .px_2()
            .child(
                Button::new(format!("sidebar-project-{}", work_dir.display()))
                    .ghost()
                    .w_full()
                    .h_auto()
                    .min_h_12()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .w_full()
                            .min_w_0()
                            .child(icon(kit_icons::Folder.1).xsmall().text_color(accent))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .gap_1()
                                    .child(
                                        div().font_bold().truncate().child(project.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .child(format!(
                                                "{} {} · {}",
                                                project.sessions.len(),
                                                if project.sessions.len() == 1 {
                                                    "chat"
                                                } else {
                                                    "chats"
                                                },
                                                status
                                            )),
                                    ),
                            )
                            .when(selected, |this| {
                                this.child(icon(kit_icons::Check.1).xsmall().text_color(accent))
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_project(work_dir.clone(), cx)
                    })),
            )
            .into_any_element()
    }
}

impl MobileApp {
    /// The Git tab: the desktop's right-panel git surface stacked into one
    /// scrolling column — sync, changes, commit, branches, stashes,
    /// commits — scoped to the sidebar-selected project.
    fn render_git_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let work_dir = self.client.sidebar_project_filter.clone();
        let project_name = work_dir
            .as_ref()
            .and_then(|dir| self.client.projects.iter().find(|p| &p.work_dir == dir))
            .map(|project| project.name.clone());
        let status = work_dir
            .as_ref()
            .and_then(|dir| self.project_git.get(dir).cloned());
        let status_line = self.client.session_status.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("git-open-sidebar")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::Menu.1))
                            .accessibility_label("Menu")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_open = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div().font_bold().truncate().child(
                                            project_name
                                                .map(|name| format!("Git — {name}"))
                                                .unwrap_or_else(|| "Git".into()),
                                        ),
                                    )
                                    .child(self.link_dot(cx)),
                            )
                            .when_some(status_line, |this, line| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(line),
                                )
                            }),
                    )
                    .child(
                        Button::new("git-refresh")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::RefreshCw.1))
                            .accessibility_label("Fetch and refresh")
                            .disabled(work_dir.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send_git(GitOperation::Inspect { sync_remote: true });
                                cx.notify();
                            })),
                    ),
            )
            .when(work_dir.is_none(), |this| {
                this.child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .px_6()
                        .child(
                            icon(kit_icons::GitBranch.1)
                                .large()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Choose a project to see its repository"),
                        )
                        .child(
                            Button::new("git-choose-project")
                                .primary()
                                .h_11()
                                .label("Choose project")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sidebar_open = true;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(work_dir, |this, _work_dir| match status {
                Some(status) => this.child(self.render_git_status(&status, cx)),
                None => this.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Loading Git status…"),
                        ),
                ),
            })
    }

    fn render_git_status(&mut self, status: &GitStatus, cx: &mut Context<Self>) -> impl IntoElement {
        let staged: Vec<&GitFile> = status.files.iter().filter(|file| file.staged).collect();
        let unstaged: Vec<&GitFile> = status
            .files
            .iter()
            .filter(|file| file.unstaged)
            .collect();
        let staged_paths: Vec<String> = staged.iter().map(|file| file.path.clone()).collect();
        let push_paths = staged_paths.clone();
        let message = self.git_message.read(cx).value().trim().to_owned();
        let commit_ready = !message.is_empty() && !staged_paths.is_empty();
        let new_branch = self.git_branch.read(cx).value().trim().to_owned();
        // Checkout targets: local branches only, current first then the
        // default branch, name-ordered after that so the menu is stable.
        // Owned because the dropdown builder closure must be 'static.
        let mut branches: Vec<GitBranchInfo> = status
            .branch_details
            .iter()
            .filter(|branch| !branch.is_remote)
            .cloned()
            .collect();
        branches.sort_by(|a, b| {
            b.is_current
                .cmp(&a.is_current)
                .then(b.is_default.cmp(&a.is_default))
                .then(a.name.cmp(&b.name))
        });
        let entity = cx.entity();
        div()
            .id("git-panel")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        icon(kit_icons::GitBranch.1)
                                            .xsmall()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .child(
                                        div().flex_1().min_w_0().truncate().font_bold().child(
                                            status
                                                .branch
                                                .clone()
                                                .unwrap_or_else(|| "Detached HEAD".into()),
                                        ),
                                    )
                                    .when(status.ahead > 0 || status.behind > 0, |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "↑{} ↓{}",
                                                    status.ahead, status.behind
                                                )),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("git-fetch")
                                            .ghost()
                                            .h_11()
                                            .flex_1()
                                            .icon(icon(kit_icons::RefreshCw.1))
                                            .label("Fetch")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send_git(GitOperation::Fetch);
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("git-pull")
                                            .ghost()
                                            .h_11()
                                            .flex_1()
                                            .icon(icon(kit_icons::Download.1))
                                            .label(if status.behind > 0 {
                                                format!("Pull ({})", status.behind)
                                            } else {
                                                "Pull".into()
                                            })
                                            .disabled(!status.has_upstream)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send_git(GitOperation::Pull);
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("git-push")
                                            .ghost()
                                            .h_11()
                                            .flex_1()
                                            .icon(icon(kit_icons::Upload.1))
                                            .label(if status.ahead > 0 {
                                                format!("Push ({})", status.ahead)
                                            } else {
                                                "Push".into()
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send_git(GitOperation::Push);
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .when_some(status.pr.clone(), |this, pr| {
                        this.child(self.render_git_pr(&pr, cx))
                    })
                    .when(
                        status.pr.is_none() && status.pr_ready && status.has_upstream,
                        |this| {
                            this.child(
                                Button::new("git-create-pr")
                                    .outline()
                                    .h_11()
                                    .icon(icon(kit_icons::GitPullRequest.1))
                                    .label("Create pull request")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send_git(GitOperation::CreatePullRequest);
                                        cx.notify();
                                    })),
                            )
                        },
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(if staged.is_empty() && unstaged.is_empty() {
                                                "Working tree clean".to_string()
                                            } else {
                                                format!("Changes ({})", staged.len() + unstaged.len())
                                            }),
                                    )
                                    .when(!unstaged.is_empty(), |this| {
                                        this.child(
                                            Button::new("git-stage-all")
                                                .ghost()
                                                .small()
                                                .label("Stage all")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.send_git(GitOperation::StageAll);
                                                    cx.notify();
                                                })),
                                        )
                                    })
                                    .when(!staged.is_empty(), |this| {
                                        this.child(
                                            Button::new("git-unstage-all")
                                                .ghost()
                                                .small()
                                                .label("Unstage all")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.send_git(GitOperation::UnstageAll);
                                                    cx.notify();
                                                })),
                                        )
                                    }),
                            )
                            .when(!staged.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .pt_1()
                                        .child("Staged"),
                                )
                            })
                            .children(staged.iter().map(|file| {
                                self.render_git_file(
                                    format!("git-file-staged-{}", file.path),
                                    file,
                                    GitOperation::DiffFile {
                                        path: file.path.clone(),
                                        options: DiffOptions::default(),
                                    },
                                    Some(GitOperation::Unstage {
                                        paths: vec![file.path.clone()],
                                    }),
                                    cx,
                                )
                            }))
                            .when(!unstaged.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .pt_1()
                                        .child("Unstaged"),
                                )
                            })
                            .children(unstaged.iter().map(|file| {
                                self.render_git_file(
                                    format!("git-file-unstaged-{}", file.path),
                                    file,
                                    GitOperation::DiffFile {
                                        path: file.path.clone(),
                                        options: DiffOptions::default(),
                                    },
                                    Some(GitOperation::Stage {
                                        paths: vec![file.path.clone()],
                                    }),
                                    cx,
                                )
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Commit"),
                            )
                            .child(
                                Textarea::new(&self.git_message)
                                    .aria_label("Commit message")
                                    .h_11(),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("git-commit")
                                            .primary()
                                            .h_11()
                                            .flex_1()
                                            .label("Commit")
                                            .disabled(!commit_ready)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                let message = this
                                                    .git_message
                                                    .read(cx)
                                                    .value()
                                                    .trim()
                                                    .to_owned();
                                                if message.is_empty() {
                                                    return;
                                                }
                                                this.send_git(GitOperation::Commit {
                                                    message,
                                                    selected_paths: staged_paths.clone(),
                                                    push: false,
                                                });
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("git-commit-push")
                                            .ghost()
                                            .h_11()
                                            .flex_1()
                                            .label("Commit & push")
                                            .disabled(!commit_ready)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                let message = this
                                                    .git_message
                                                    .read(cx)
                                                    .value()
                                                    .trim()
                                                    .to_owned();
                                                if message.is_empty() {
                                                    return;
                                                }
                                                this.send_git(GitOperation::Commit {
                                                    message,
                                                    selected_paths: push_paths.clone(),
                                                    push: true,
                                                });
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Branches"),
                            )
                            .child(
                                Button::new("git-branch-menu")
                                    .outline()
                                    .h_11()
                                    .w_full()
                                    .label(format!(
                                        "{} ▾",
                                        status.branch.as_deref().unwrap_or("Detached")
                                    ))
                                    .accessibility_label("Switch branch")
                                    .dropdown_caret(true)
                                    .dropdown_menu_with_anchor(Anchor::BottomLeft, {
                                        move |menu, _, _| {
                                            branches.iter().fold(menu.scrollable(true), |menu, branch| {
                                                let entity = entity.clone();
                                                let name = branch.name.clone();
                                                menu.item(
                                                    PopupMenuItem::new(branch.name.clone())
                                                        .checked(branch.is_current)
                                                        .on_click(move |_, _, cx| {
                                                            let _ = entity.update(cx, |this, cx| {
                                                                this.send_git(GitOperation::Checkout {
                                                                    branch: name.clone(),
                                                                    mode: CheckoutMode::Clean,
                                                                });
                                                                cx.notify();
                                                            });
                                                        }),
                                                )
                                            })
                                        }
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        div().flex_1().min_w_0().child(
                                            Input::new(&self.git_branch)
                                                .aria_label("New branch name")
                                                .h_11(),
                                        ),
                                    )
                                    .child(
                                        Button::new("git-create-branch")
                                            .outline()
                                            .h_11()
                                            .icon(icon(kit_icons::Plus.1))
                                            .label("Create")
                                            .disabled(new_branch.is_empty())
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                let name = this
                                                    .git_branch
                                                    .read(cx)
                                                    .value()
                                                    .trim()
                                                    .to_owned();
                                                if name.is_empty() {
                                                    return;
                                                }
                                                this.send_git(GitOperation::CreateBranch { name });
                                                this.git_branch.update(cx, |input, cx| {
                                                    input.set_value("", window, cx)
                                                });
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(if status.stashes.is_empty() {
                                                "Stash".to_string()
                                            } else {
                                                format!("Stashes ({})", status.stashes.len())
                                            }),
                                    )
                                    .child(
                                        Button::new("git-stash-push")
                                            .ghost()
                                            .small()
                                            .icon(icon(kit_icons::Inbox.1))
                                            .label("Stash changes")
                                            .disabled(!status.has_changes)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send_git(GitOperation::StashPush {
                                                    message: None,
                                                    include_untracked: true,
                                                });
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .children(
                                status
                                    .stashes
                                    .iter()
                                    .map(|stash| self.render_stash_row(stash, cx)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Recent commits"),
                            )
                            .when(status.recent_commits.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No commits yet"),
                                )
                            })
                            .children(
                                status
                                    .recent_commits
                                    .iter()
                                    .map(|commit| self.render_commit_row(commit, cx)),
                            ),
                    ),
            )
    }

    fn render_git_pr(&self, pr: &GitHubPrInfo, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                icon(kit_icons::GitPullRequest.1)
                    .xsmall()
                    .text_color(cx.theme().accent),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .font_bold()
                            .truncate()
                            .child(format!("#{} — {}", pr.number, pr.title)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{}{} · {} checks{}",
                                if pr.is_draft { "Draft " } else { "" },
                                pr.state,
                                pr.passing_checks,
                                if pr.total_checks > 0 {
                                    format!("/{} passing", pr.total_checks)
                                } else {
                                    String::new()
                                }
                            )),
                    ),
            )
    }

    /// One file row: tap opens the full-screen diff; the trailing button
    /// carries the stage/unstage action when the row is a working-tree
    /// file (commit and stash rows pass `None`).
    fn render_git_file(
        &self,
        id: String,
        file: &GitFile,
        diff: GitOperation,
        action: Option<GitOperation>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = file.path.clone();
        let action_key = file.path.clone();
        let (action_label, action_verb) = match &action {
            Some(GitOperation::Stage { .. }) => (Some("stage"), Some("Stage")),
            Some(GitOperation::Unstage { .. }) => (Some("unstage"), Some("Unstage")),
            _ => (None, None),
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new(id)
                    .ghost()
                    .flex_1()
                    .h_auto()
                    .min_h_10()
                    .child(self.render_git_file_label(file, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_file_diff(title.clone(), diff.clone(), cx);
                    })),
            )
            .when_some(action, |this, action| {
                this.child(
                    Button::new(format!("git-action-{}-{action_key}", action_label.unwrap_or("noop")))
                        .ghost()
                        .small()
                        .h_9()
                        .label(action_verb.unwrap_or_default())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send_git(action.clone());
                            cx.notify();
                        })),
                )
            })
    }

    fn render_git_file_label(&self, file: &GitFile, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .w_4()
                    .flex_none()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(git_status_color(file.status_char(), cx))
                    .child(file.status_char().to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_start()
                    .child(div().text_sm().truncate().child(file.path.clone()))
                    .when(file.additions + file.deletions > 0, |this| {
                        this.child(
                            div()
                                .flex()
                                .gap_1()
                                .text_xs()
                                .child(
                                    div()
                                        .text_color(cx.theme().success)
                                        .child(format!("+{}", file.additions)),
                                )
                                .child(
                                    div()
                                        .text_color(cx.theme().danger)
                                        .child(format!("-{}", file.deletions)),
                                ),
                        )
                    }),
            )
    }

    fn render_stash_row(&self, stash: &GitStashInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let key = format!("stash-{}", stash.index);
        let index = stash.index;
        let scoped = self.git_object_key(&key);
        let expanded = self.git_expanded.contains(&scoped);
        let files = self.git_object_files.get(&scoped).cloned();
        let title = if stash.message.is_empty() {
            stash.name.clone()
        } else {
            stash.message.clone()
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(format!("git-{key}"))
                            .ghost()
                            .flex_1()
                            .h_auto()
                            .min_h_10()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .w_full()
                                    .min_w_0()
                                    .child(
                                        icon(kit_icons::Inbox.1)
                                            .xsmall()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .items_start()
                                            .child(div().text_sm().truncate().child(title))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(stash.relative_time.clone()),
                                            ),
                                    )
                                    .child(
                                        icon(if expanded {
                                            kit_icons::ChevronDown.1
                                        } else {
                                            kit_icons::ChevronRight.1
                                        })
                                        .xsmall()
                                        .text_color(cx.theme().muted_foreground),
                                    ),
                            )
                            .accessibility_label(format!(
                                "{} files in {}",
                                if expanded { "Hide" } else { "Show" },
                                stash.name
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let key = this.git_object_key(&key);
                                this.toggle_git_object(
                                    key,
                                    GitOperation::StashFiles { index },
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new(format!("git-stash-pop-{index}"))
                            .ghost()
                            .small()
                            .h_9()
                            .label("Pop")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_git(GitOperation::PopStash {
                                    index: Some(index),
                                });
                                cx.notify();
                            })),
                    )
                    .child({
                        let armed = self.stash_drop_confirm == Some(index);
                        let in_flight = self.stash_drop_inflight.is_some();
                        // Dropping a stash destroys unmerged work: arm it
                        // with a first tap, and serialize sends so a fast
                        // second tap cannot delete the stash that shifts
                        // into this index after the first drop lands.
                        Button::new(format!("git-stash-drop-{index}"))
                            .ghost()
                            .small()
                            .h_9()
                            .label(if in_flight {
                                "Dropping…"
                            } else if armed {
                                "Drop?"
                            } else {
                                "Drop"
                            })
                            .disabled(in_flight)
                            .accessibility_label(if armed {
                                format!("Confirm drop of {}", stash.name)
                            } else {
                                format!("Drop {}", stash.name)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.stash_drop_inflight.is_some() {
                                    return;
                                }
                                if this.stash_drop_confirm == Some(index) {
                                    this.stash_drop_confirm = None;
                                    this.stash_drop_inflight = Some(index);
                                    this.send_git(GitOperation::DropStash {
                                        index: Some(index),
                                    });
                                } else {
                                    this.stash_drop_confirm = Some(index);
                                }
                                cx.notify();
                            }))
                    }),
            )
            .when(expanded, |this| {
                this.child(
                    div().pl_6().flex().flex_col().children(match files {
                        Some(files) if files.is_empty() => vec![div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("No files")
                            .into_any_element()],
                        Some(files) => files
                            .iter()
                            .enumerate()
                            .map(|(ix, file)| {
                                self.render_git_file(
                                    format!("git-stash-file-{index}-{ix}"),
                                    file,
                                    GitOperation::DiffStashFile {
                                        index,
                                        path: file.path.clone(),
                                    },
                                    None,
                                    cx,
                                )
                                .into_any_element()
                            })
                            .collect(),
                        None => vec![div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Loading…")
                            .into_any_element()],
                    }),
                )
            })
    }

    fn render_commit_row(&self, commit: &GitCommitInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let key = format!("commit-{}", commit.sha);
        let scoped = self.git_object_key(&key);
        let expanded = self.git_expanded.contains(&scoped);
        let files = self.git_object_files.get(&scoped).cloned();
        let sha = commit.sha.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Button::new(format!("git-{key}"))
                    .ghost()
                    .w_full()
                    .h_auto()
                    .min_h_12()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .w_full()
                            .min_w_0()
                            .child(
                                icon(kit_icons::GitCommitHorizontal.1)
                                    .xsmall()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .child(
                                        div().text_sm().truncate().child(commit.summary.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .child(format!(
                                                "{} · {} · {}",
                                                commit.short_sha,
                                                commit.author_name,
                                                commit.relative_time
                                            )),
                                    ),
                            )
                            .child(
                                icon(if expanded {
                                    kit_icons::ChevronDown.1
                                } else {
                                    kit_icons::ChevronRight.1
                                })
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                            ),
                    )
                    .accessibility_label(format!(
                        "{} files in commit {}",
                        if expanded { "Hide" } else { "Show" },
                        commit.short_sha
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let key = this.git_object_key(&key);
                        this.toggle_git_object(
                            key,
                            GitOperation::CommitFiles { sha: sha.clone() },
                            cx,
                        );
                    })),
            )
            .when(expanded, |this| {
                this.child(
                    div().pl_6().flex().flex_col().children(match files {
                        Some(files) if files.is_empty() => vec![div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("No files")
                            .into_any_element()],
                        Some(files) => files
                            .iter()
                            .enumerate()
                            .map(|(ix, file)| {
                                self.render_git_file(
                                    format!("git-commit-file-{}-{ix}", commit.sha),
                                    file,
                                    GitOperation::DiffCommitFile {
                                        sha: commit.sha.clone(),
                                        path: file.path.clone(),
                                    },
                                    None,
                                    cx,
                                )
                                .into_any_element()
                            })
                            .collect(),
                        None => vec![div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Loading…")
                            .into_any_element()],
                    }),
                )
            })
    }

    /// Full-screen unified diff — per-line tinting mirrors the desktop's
    /// diff rows without dragging its tool-detail machinery into mobile.
    fn render_git_diff(&self, title: &str, text: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let rows = if text.is_empty() {
            vec![div()
                .w_full()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("No changes".to_string())
                .into_any_element()]
        } else {
            text.lines()
                .enumerate()
                .map(|(ix, line)| {
                    let (tint, color) = if line.starts_with("+++") || line.starts_with("---") {
                        (theme.muted_foreground.opacity(0.0), theme.muted_foreground)
                    } else if line.starts_with('+') {
                        (theme.success.opacity(0.10), theme.success)
                    } else if line.starts_with('-') {
                        (theme.danger.opacity(0.10), theme.danger)
                    } else if line.starts_with("@@") {
                        (theme.info.opacity(0.10), theme.info)
                    } else if line.starts_with("diff --git") || line.starts_with("index ") {
                        (theme.muted.opacity(0.25), theme.muted_foreground)
                    } else {
                        (theme.muted_foreground.opacity(0.0), theme.foreground)
                    };
                    div()
                        .id(("git-diff-row", ix))
                        .w_full()
                        .px_3()
                        .py_0p5()
                        .bg(tint)
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(color)
                        .whitespace_nowrap()
                        .child(line.to_string())
                        .into_any_element()
                })
                .collect()
        };
        div()
            .id("git-diff")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("close-diff")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::X.1))
                            .accessibility_label("Close diff")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.git_diff = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_bold()
                            .child(title.to_string()),
                    ),
            )
            .child(
                div()
                    .id("git-diff-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .overflow_x_scrollbar()
                    .child(div().flex().flex_col().py_1().children(rows)),
            )
    }
}

impl MobileApp {
    /// Shared panel header: drawer button, title + link state, a refresh
    /// affordance. `refresh` is the panel's own list reload so the header
    /// stays one shape.
    fn github_panel_header(
        &self,
        title: String,
        refresh: fn(&Self),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status_line = self.client.session_status.clone();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new(format!("panel-menu-{title}"))
                    .ghost()
                    .h_11()
                    .icon(icon(kit_icons::Menu.1))
                    .accessibility_label("Menu")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = true;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().font_bold().truncate().child(title.clone()))
                            .child(self.link_dot(cx)),
                    )
                    .when_some(status_line, |this, line| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(line),
                        )
                    }),
            )
            .child(
                Button::new(format!("panel-refresh-{title}"))
                    .ghost()
                    .h_11()
                    .icon(icon(kit_icons::RefreshCw.1))
                    .accessibility_label("Refresh")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        refresh(this);
                        cx.notify();
                    })),
            )
    }

    /// Empty state when no project is selected — every project-scoped
    /// panel lands here until the sidebar pick resolves.
    fn render_choose_project(
        &self,
        id: &'static str,
        glyph: &'static [u8],
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(
                icon(glyph)
                    .large()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(hint),
            )
            .child(
                Button::new(id)
                    .primary()
                    .h_11()
                    .label("Choose project")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = true;
                        cx.notify();
                    })),
            )
    }

    fn render_issues_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let work_dir = self.client.sidebar_project_filter.clone();
        let filter = self.issue_filter;
        let entity = cx.entity();
        let states = [GitHubIssueListState::Open, GitHubIssueListState::Closed];
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.github_panel_header("Issues".into(), Self::refresh_issues, cx))
            .when(work_dir.is_some(), |this| {
                this.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .child(
                            Button::new("issue-state-filter")
                                .outline()
                                .h_10()
                                .label(format!("{} ▾", filter.as_str()))
                                .accessibility_label(format!("Issue filter: {}", filter.as_str()))
                                .dropdown_caret(true)
                                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                                    states.iter().fold(menu, |menu, state| {
                                        let entity = entity.clone();
                                        let state = *state;
                                        menu.item(
                                            PopupMenuItem::new(state.as_str().to_string())
                                                .checked(state == filter)
                                                .on_click(move |_, _, cx| {
                                                    let _ = entity.update(cx, |this, cx| {
                                                        this.issue_filter = state;
                                                        this.issues = None;
                                                        this.refresh_issues();
                                                        cx.notify();
                                                    });
                                                }),
                                        )
                                    })
                                }),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(&self.issue_search)
                                    .aria_label("Search issues")
                                    .h_10(),
                            ),
                        )
                        .child(
                            Button::new("issue-search-go")
                                .ghost()
                                .h_10()
                                .icon(icon(kit_icons::Search.1))
                                .accessibility_label("Search")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let query = this.issue_search.read(cx).value().trim().to_owned();
                                    this.send_github(GitHubOperation::ListIssues {
                                        state: this.issue_filter,
                                        query: (!query.is_empty()).then_some(query),
                                        limit: 30,
                                    });
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("issue-new")
                                .primary()
                                .h_10()
                                .icon(icon(kit_icons::Plus.1))
                                .accessibility_label("New issue")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.new_issue_open = true;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                div()
                    .id("issue-list")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .when(work_dir.is_none(), |this| {
                        this.child(self.render_choose_project(
                            "issues-choose",
                            kit_icons::CircleDot.1,
                            "Choose a project to see its issues",
                            cx,
                        ))
                    })
                    .when(work_dir.is_some() && self.issues.is_none(), |this| {
                        this.child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Loading issues…"),
                                ),
                        )
                    })
                    .when_some(self.issues.clone(), |this, issues| {
                        if issues.is_empty() {
                            this.child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("No {} issues", filter.as_str())),
                                    ),
                            )
                        } else {
                            this.children(
                                issues
                                    .iter()
                                    .map(|issue| self.render_issue_row(issue, cx)),
                            )
                        }
                    }),
            )
    }

    fn render_issue_row(
        &self,
        issue: &GitHubIssueSummary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let number = issue.issue.number;
        let open = issue.state.eq_ignore_ascii_case("open");
        let dot = if open {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };
        div().px_2().py_1().child(
            Button::new(format!("issue-row-{number}"))
                .ghost()
                .w_full()
                .h_auto()
                .min_h_12()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .w_full()
                        .min_w_0()
                        .child(
                            icon(kit_icons::CircleDot.1)
                                .xsmall()
                                .text_color(dot),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .items_start()
                                .gap_1()
                                .child(
                                    div().text_sm().font_bold().truncate().child(issue.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(format!(
                                            "#{number} · {} · {}",
                                            issue.author, issue.updated_at
                                        )),
                                ),
                        )
                        .children(issue.labels.iter().take(2).map(|label| {
                            Tag::new().small().child(label.name.clone()).into_any_element()
                        }))
                        .when(issue.comments_count > 0, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        icon(kit_icons::MessageSquare.1)
                                            .xsmall()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(issue.comments_count.to_string()),
                                    ),
                            )
                        }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.issue_detail_pending = Some(number);
                    this.issue_confirm_delete = false;
                    this.send_github(GitHubOperation::InspectIssue { number });
                    cx.notify();
                })),
        )
    }

    /// Issue detail as a full-screen overlay: body, labels, the comment
    /// thread, and a footer with the comment composer and state actions.
    fn render_issue_detail(
        &mut self,
        detail: &GitHubIssueDetail,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let issue = &detail.summary;
        let number = issue.issue.number;
        let open = issue.state.eq_ignore_ascii_case("open");
        let pending = self.issue_detail_pending == Some(number);
        let confirming = self.issue_confirm_delete;
        let comment_ready = !self.issue_comment.read(cx).value().trim().is_empty();
        div()
            .id("issue-detail")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("issue-detail-close")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::X.1))
                            .accessibility_label("Close issue")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.stash_comment_drafts(cx);
                                this.issue_detail = None;
                                this.issue_detail_pending = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_bold()
                            .child(format!("#{number} — {}", issue.title)),
                    )
                    .child(Tag::new().small().child(issue.state.clone()))
                    .when(pending, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("…"),
                        )
                    }),
            )
            .child(
                div()
                    .id("issue-detail-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "Opened by {} · updated {}",
                                        issue.author, issue.updated_at
                                    )),
                            )
                            .when(!issue.labels.is_empty(), |this| {
                                this.child(
                                    div().flex().gap_1().flex_wrap().children(
                                        issue.labels.iter().map(|label| {
                                            Tag::new().small().child(label.name.clone())
                                        }),
                                    ),
                                )
                            })
                            .child(
                                div()
                                    .text_sm()
                                    .child(if detail.body.trim().is_empty() {
                                        "No description.".to_string()
                                    } else {
                                        detail.body.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .pt_2()
                                    .child(format!("Comments ({})", detail.comments.len())),
                            )
                            .children(detail.comments.iter().map(|comment| {
                                div()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .rounded_md()
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} · {}",
                                                comment.author, comment.created_at
                                            )),
                                    )
                                    .child(div().text_sm().child(comment.body.clone()))
                            })),
                    ),
            )
            .when(confirming, |this| {
                this.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .bg(cx.theme().danger.opacity(0.08))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(format!("Delete issue #{number}? This cannot be undone.")),
                        )
                        .child(
                            Button::new("issue-delete-confirm")
                                .danger()
                                .small()
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.issue_confirm_delete = false;
                                    this.send_github(GitHubOperation::DeleteIssue { number });
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("issue-delete-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.issue_confirm_delete = false;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Textarea::new(&self.issue_comment).aria_label("Add a comment"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("issue-comment-send")
                                    .primary()
                                    .h_11()
                                    .flex_1()
                                    .label("Comment")
                                    .disabled(!comment_ready)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let body = this
                                            .issue_comment
                                            .read(cx)
                                            .value()
                                            .trim()
                                            .to_owned();
                                        if body.is_empty() {
                                            return;
                                        }
                                        this.send_github(GitHubOperation::CommentIssue {
                                            number,
                                            body,
                                        });
                                        this.issue_comment.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("issue-toggle-state")
                                    .outline()
                                    .h_11()
                                    .label(if open { "Close" } else { "Reopen" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send_github(GitHubOperation::SetIssueState {
                                            number,
                                            close: open,
                                        });
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("issue-delete")
                                    .danger()
                                    .h_11()
                                    .icon(icon(kit_icons::Trash.1))
                                    .accessibility_label("Delete issue")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.issue_confirm_delete = true;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
    }

    /// The new-issue form — another overlay, since the composer belongs
    /// over the list it will update.
    fn render_new_issue(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let title_ready = !self.issue_title.read(cx).value().trim().is_empty();
        div()
            .id("new-issue")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("new-issue-close")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::X.1))
                            .accessibility_label("Cancel new issue")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.new_issue_open = false;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().font_bold().child("New issue")),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        Input::new(&self.issue_title)
                            .aria_label("Issue title")
                            .h_11(),
                    )
                    .child(Textarea::new(&self.issue_body).aria_label("Issue body")),
            )
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("new-issue-create")
                            .primary()
                            .h_11()
                            .flex_1()
                            .label("Create issue")
                            .disabled(!title_ready)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                let title = this.issue_title.read(cx).value().trim().to_owned();
                                if title.is_empty() {
                                    return;
                                }
                                let body = this.issue_body.read(cx).value().trim().to_owned();
                                this.send_github(GitHubOperation::CreateIssue { title, body });
                                // Fields keep their text until the `Number`
                                // reply confirms creation — a failed send
                                // must not wipe the user's draft.
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_pulls_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let work_dir = self.client.sidebar_project_filter.clone();
        let filter = self.pr_filter;
        let entity = cx.entity();
        let states = [
            GitHubPrListState::Open,
            GitHubPrListState::Closed,
            GitHubPrListState::Merged,
        ];
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.github_panel_header("Pull requests".into(), Self::refresh_prs, cx))
            .when(work_dir.is_some(), |this| {
                this.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .child(
                            Button::new("pr-state-filter")
                                .outline()
                                .h_10()
                                .label(format!("{} ▾", filter.as_str()))
                                .accessibility_label(format!("Pull request filter: {}", filter.as_str()))
                                .dropdown_caret(true)
                                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                                    states.iter().fold(menu, |menu, state| {
                                        let entity = entity.clone();
                                        let state = *state;
                                        menu.item(
                                            PopupMenuItem::new(state.as_str().to_string())
                                                .checked(state == filter)
                                                .on_click(move |_, _, cx| {
                                                    let _ = entity.update(cx, |this, cx| {
                                                        this.pr_filter = state;
                                                        this.prs = None;
                                                        this.refresh_prs();
                                                        cx.notify();
                                                    });
                                                }),
                                        )
                                    })
                                }),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(&self.pr_search)
                                    .aria_label("Search pull requests")
                                    .h_10(),
                            ),
                        )
                        .child(
                            Button::new("pr-search-go")
                                .ghost()
                                .h_10()
                                .icon(icon(kit_icons::Search.1))
                                .accessibility_label("Search")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let query = this.pr_search.read(cx).value().trim().to_owned();
                                    this.send_github(GitHubOperation::ListPullRequests {
                                        state: this.pr_filter,
                                        query: (!query.is_empty()).then_some(query),
                                        limit: 30,
                                    });
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                div()
                    .id("pr-list")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .when(work_dir.is_none(), |this| {
                        this.child(self.render_choose_project(
                            "prs-choose",
                            kit_icons::GitPullRequest.1,
                            "Choose a project to see its pull requests",
                            cx,
                        ))
                    })
                    .when(work_dir.is_some() && self.prs.is_none(), |this| {
                        this.child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Loading pull requests…"),
                                ),
                        )
                    })
                    .when_some(self.prs.clone(), |this, prs| {
                        if prs.is_empty() {
                            this.child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("No {} pull requests", filter.as_str())),
                                    ),
                            )
                        } else {
                            this.children(prs.iter().map(|pr| self.render_pr_row(pr, cx)))
                        }
                    }),
            )
    }

    fn render_pr_row(
        &self,
        pr: &GitHubPullRequestSummary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let number = pr.number;
        let glyph = if pr.state.eq_ignore_ascii_case("merged") {
            cx.theme().info
        } else if pr.state.eq_ignore_ascii_case("open") {
            cx.theme().success
        } else {
            cx.theme().muted_foreground
        };
        let (passing, total) = pr
            .checks
            .iter()
            .fold((0usize, 0usize), |(ok, all), check| {
                (
                    ok + usize::from(check_state(check) == CheckState::Passing),
                    all + 1,
                )
            });
        div().px_2().py_1().child(
            Button::new(format!("pr-row-{number}"))
                .ghost()
                .w_full()
                .h_auto()
                .min_h_12()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .w_full()
                        .min_w_0()
                        .child(icon(kit_icons::GitPullRequest.1).xsmall().text_color(glyph))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .items_start()
                                .gap_1()
                                .child(
                                    div().text_sm().font_bold().truncate().child(pr.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(format!(
                                            "#{number} · {} → {} · {} · {}",
                                            pr.head_ref, pr.base_ref, pr.author, pr.updated_at
                                        )),
                                ),
                        )
                        .when(pr.is_draft, |this| {
                            this.child(Tag::new().small().child("Draft"))
                        })
                        .when(total > 0, |this| {
                            let ok = passing == total;
                            this.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        icon(if ok {
                                            kit_icons::CircleCheck.1
                                        } else {
                                            kit_icons::CircleX.1
                                        })
                                        .xsmall()
                                        .text_color(if ok {
                                            cx.theme().success
                                        } else {
                                            cx.theme().danger
                                        }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if ok {
                                                cx.theme().success
                                            } else {
                                                cx.theme().danger
                                            })
                                            .child(format!("{passing}/{total}")),
                                    ),
                            )
                        }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pr_detail_pending = Some(number);
                    this.send_github(GitHubOperation::InspectPullRequest { number });
                    cx.notify();
                })),
        )
    }

    /// Pull-request detail overlay: state tags, refs, checks, files, the
    /// conversation, a View-diff button, and a comment composer.
    fn render_pr_detail(&mut self, pr: &GitHubPrInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let number = pr.number;
        let pending = self.pr_detail_pending == Some(number);
        let comment_ready = !self.pr_comment.read(cx).value().trim().is_empty();
        div()
            .id("pr-detail")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("pr-detail-close")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::X.1))
                            .accessibility_label("Close pull request")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.stash_comment_drafts(cx);
                                this.pr_detail = None;
                                this.pr_detail_pending = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_bold()
                            .child(format!("#{number} — {}", pr.title)),
                    )
                    .when(pending, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("…"),
                        )
                    }),
            )
            .child(
                div()
                    .id("pr-detail-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .flex_wrap()
                                    .child(Tag::new().small().child(pr.state.clone()))
                                    .when(pr.is_draft, |this| {
                                        this.child(Tag::new().small().child("Draft"))
                                    })
                                    .when_some(pr.review_decision.clone(), |this, decision| {
                                        this.child(Tag::new().small().child(decision))
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{} → {} · {} · updated {} · {} comments",
                                        pr.head_ref,
                                        pr.base_ref,
                                        pr.author,
                                        pr.updated_at,
                                        pr.comments_count
                                    )),
                            )
                            .child(
                                Button::new("pr-view-diff")
                                    .outline()
                                    .h_11()
                                    .icon(icon(kit_icons::FileDiff.1))
                                    .label("View diff")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.git_diff_pending =
                                            Some(format!("PR #{number} diff"));
                                        this.send_github(GitHubOperation::PullRequestDiff {
                                            number,
                                        });
                                        cx.notify();
                                    })),
                            )
                            .when(!pr.body.trim().is_empty(), |this| {
                                this.child(div().text_sm().child(pr.body.clone()))
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .pt_2()
                                    .child(format!("Checks ({})", pr.checks.len())),
                            )
                            .when(pr.checks.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No checks reported"),
                                )
                            })
                            .children(pr.checks.iter().map(|check| {
                                let (glyph, color) = match check_state(check) {
                                    CheckState::Passing => {
                                        (kit_icons::CircleCheck.1, cx.theme().success)
                                    }
                                    CheckState::Failing => {
                                        (kit_icons::CircleX.1, cx.theme().danger)
                                    }
                                    CheckState::Pending => {
                                        (kit_icons::Loader.1, cx.theme().muted_foreground)
                                    }
                                };
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(icon(glyph).xsmall().text_color(color))
                                    .child(div().flex_1().min_w_0().text_sm().truncate().child(
                                        check.name.clone(),
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                check
                                                    .conclusion
                                                    .clone()
                                                    .unwrap_or_else(|| check.status.clone()),
                                            ),
                                    )
                            }))
                            .when(!pr.files.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .pt_2()
                                        .child(format!("Files ({})", pr.files.len())),
                                )
                            })
                            .children(pr.files.iter().map(|file| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .truncate()
                                            .child(file.path.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().success)
                                            .child(format!("+{}", file.additions)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().danger)
                                            .child(format!("-{}", file.deletions)),
                                    )
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .pt_2()
                                    .child(format!("Conversation ({})", pr.issue_comments.len())),
                            )
                            .when(pr.issue_comments.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No comments yet"),
                                )
                            })
                            .children(pr.issue_comments.iter().map(|comment| {
                                div()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .rounded_md()
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} · {}",
                                                comment.author, comment.created_at
                                            )),
                                    )
                                    .child(div().text_sm().child(comment.body.clone()))
                            }))
                            .when(!pr.reviews.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .pt_2()
                                        .child(format!("Reviews ({})", pr.reviews.len())),
                                )
                            })
                            .children(pr.reviews.iter().map(|review| {
                                div()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .rounded_md()
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(Tag::new().small().child(review.state.clone()))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "{} · {}",
                                                        review.author, review.submitted_at
                                                    )),
                                            ),
                                    )
                                    .when(!review.body.trim().is_empty(), |this| {
                                        this.child(div().text_sm().child(review.body.clone()))
                                    })
                            })),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(Textarea::new(&self.pr_comment).aria_label("Add a comment"))
                    .child(
                        Button::new("pr-comment-send")
                            .primary()
                            .h_11()
                            .w_full()
                            .label("Comment")
                            .disabled(!comment_ready)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let body = this.pr_comment.read(cx).value().trim().to_owned();
                                if body.is_empty() {
                                    return;
                                }
                                this.send_github(GitHubOperation::CommentPullRequest {
                                    number,
                                    body,
                                });
                                this.pr_comment.update(cx, |input, cx| {
                                    input.set_value("", window, cx)
                                });
                                cx.notify();
                            })),
                    ),
            )
    }
}

impl MobileApp {
    /// The automations tab mirrors the daemon-global store: definitions
    /// with enable/run/edit/delete controls, then the run history with
    /// cancel/review/open-chat affordances.
    fn render_automations_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.client.automation.snapshot.clone();
        let attention: HashSet<String> = self
            .client
            .automation_permissions
            .keys()
            .chain(self.client.automation_questions.keys())
            .cloned()
            .collect();
        let delete_def = self.auto_delete_confirm.clone();
        let delete_run = self.auto_run_delete_confirm.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                self.github_panel_header(
                    "Automations".into(),
                    Self::refresh_automations,
                    cx,
                ),
            )
            .child(
                div().flex_none().px_3().py_2().child(
                    Button::new("auto-new")
                        .primary()
                        .h_10()
                        .w_full()
                        .icon(icon(kit_icons::Plus.1))
                        .label("New automation")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_auto_form(None, window, cx)
                        })),
                ),
            )
            .when_some(delete_def, |this, id| {
                this.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .bg(cx.theme().danger.opacity(0.08))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child("Delete this automation and its runs?"),
                        )
                        .child(
                            Button::new("auto-delete-confirm")
                                .danger()
                                .small()
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.auto_delete_confirm = None;
                                    this.send_automation(AutomationCommand::Delete {
                                        id: id.clone(),
                                    });
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("auto-delete-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.auto_delete_confirm = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(delete_run, |this, id| {
                this.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .bg(cx.theme().danger.opacity(0.08))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child("Delete this run? An active run is cancelled."),
                        )
                        .child(
                            Button::new("auto-run-delete-confirm")
                                .danger()
                                .small()
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.auto_run_delete_confirm = None;
                                    this.send_automation(AutomationCommand::DeleteRun {
                                        id: id.clone(),
                                    });
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("auto-run-delete-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.auto_run_delete_confirm = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                div()
                    .id("auto-list")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .px_3()
                            .pb_4()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .pt_1()
                                    .child(format!("Definitions ({})", snapshot.definitions.len())),
                            )
                            .when(snapshot.definitions.is_empty(), |this| {
                                this.child(
                                    div()
                                        .py_6()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            icon(kit_icons::Zap.1)
                                                .large()
                                                .text_color(cx.theme().muted_foreground),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("No automations yet — create one above"),
                                        ),
                                )
                            })
                            .children(
                                snapshot
                                    .definitions
                                    .iter()
                                    .map(|def| self.render_auto_definition(def, cx)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .pt_2()
                                    .child(format!("Runs ({})", snapshot.runs.len())),
                            )
                            .when(snapshot.runs.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No runs recorded"),
                                )
                            })
                            .children(
                                snapshot
                                    .runs
                                    .iter()
                                    .map(|run| self.render_auto_run(run, cx, &attention)),
                            ),
                    ),
            )
    }

    fn render_auto_definition(
        &self,
        def: &Definition,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let toggle_id = def.id.clone();
        let run_id = def.id.clone();
        let delete_id = def.id.clone();
        let enabled = def.enabled;
        let definition = def.clone();
        let project_name = def
            .project
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let meta = format!(
            "{} · {}{}{}",
            def.schedule.label(),
            project_name,
            if def.failures > 0 {
                format!(" · {} failures", def.failures)
            } else {
                String::new()
            },
            def.next_at
                .map(|at| format!(
                    " · next {}",
                    threadlane_protocol::automation::display_time(at, "UTC")
                ))
                .unwrap_or_default()
        );
        div()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        icon(if def.enabled {
                            kit_icons::Zap.1
                        } else {
                            kit_icons::ZapOff.1
                        })
                        .xsmall()
                        .text_color(if def.enabled {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground
                        }),
                    )
                    .child(div().flex_1().min_w_0().font_bold().truncate().child(
                        if def.name.trim().is_empty() {
                            "Untitled automation".to_string()
                        } else {
                            def.name.clone()
                        },
                    ))
                    .when_some(def.paused_reason.clone(), |this, reason| {
                        this.child(Tag::new().small().child(reason))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(meta),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new(format!("auto-toggle-{}", def.id))
                            .ghost()
                            .small()
                            .h_10()
                            .icon(icon(if enabled {
                                kit_icons::ZapOff.1
                            } else {
                                kit_icons::Zap.1
                            }))
                            .label(if enabled { "Disable" } else { "Enable" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_automation(AutomationCommand::SetEnabled {
                                    id: toggle_id.clone(),
                                    enabled: !enabled,
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(format!("auto-run-{}", def.id))
                            .outline()
                            .small()
                            .h_10()
                            .icon(icon(kit_icons::Play.1))
                            .label("Run now")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_automation(AutomationCommand::RunNow {
                                    id: run_id.clone(),
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(format!("auto-edit-{}", def.id))
                            .ghost()
                            .small()
                            .h_10()
                            .label("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_auto_form(Some(definition.clone()), window, cx)
                            })),
                    )
                    .child(
                        Button::new(format!("auto-delete-{}", def.id))
                            .ghost()
                            .small()
                            .h_10()
                            .icon(icon(kit_icons::Trash.1))
                            .accessibility_label("Delete automation")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.auto_delete_confirm = Some(delete_id.clone());
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_auto_run(
        &self,
        run: &Run,
        cx: &mut Context<Self>,
        attention: &HashSet<String>,
    ) -> impl IntoElement {
        let cancel_id = run.id.clone();
        let review_id = run.id.clone();
        let delete_id = run.id.clone();
        let active = run.status.active();
        let needs_you = attention.contains(&run.session_id)
            || matches!(
                run.status,
                RunStatus::WaitingPermission | RunStatus::WaitingAnswer
            );
        let (glyph, color) = match run.status {
            RunStatus::Succeeded => (kit_icons::CircleCheck.1, cx.theme().success),
            RunStatus::Failed => (kit_icons::CircleX.1, cx.theme().danger),
            RunStatus::Cancelled | RunStatus::Interrupted => {
                (kit_icons::Ban.1, cx.theme().muted_foreground)
            }
            RunStatus::WaitingPermission | RunStatus::WaitingAnswer => {
                (kit_icons::TriangleAlert.1, cx.theme().warning)
            }
            _ => (kit_icons::Loader.1, cx.theme().info),
        };
        let times = format!(
            "started {}{}",
            threadlane_protocol::automation::display_time(run.created_at, "UTC"),
            run.finished_at
                .map(|at| format!(
                    " · finished {}",
                    threadlane_protocol::automation::display_time(at, "UTC")
                ))
                .unwrap_or_default()
        );
        let session = self
            .client
            .projects
            .iter()
            .flat_map(|project| &project.sessions)
            .find(|session| session.id == run.session_id)
            .cloned();
        div()
            .border_1()
            .border_color(if needs_you {
                cx.theme().warning
            } else {
                cx.theme().border
            })
            .rounded_md()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(glyph).xsmall().text_color(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div().text_sm().font_bold().truncate().child(
                                    if run.definition.name.trim().is_empty() {
                                        run.definition.id.clone()
                                    } else {
                                        run.definition.name.clone()
                                    },
                                ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{} · {}", run.status.label(), times)),
                            ),
                    )
                    .when(needs_you, |this| {
                        this.child(Tag::new().small().child("Needs you"))
                    })
                    .when(
                        !run.reviewed
                            && matches!(
                                run.status,
                                RunStatus::Succeeded
                                    | RunStatus::Failed
                                    | RunStatus::Interrupted
                            ),
                        |this| this.child(Tag::new().small().child("Unreviewed")),
                    ),
            )
            .when_some(run.error.clone(), |this, error| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .when(active, |this| {
                        this.child(
                            Button::new(format!("auto-run-cancel-{}", run.id))
                                .ghost()
                                .small()
                                .h_10()
                                .icon(icon(kit_icons::Ban.1))
                                .label("Cancel")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send_automation(AutomationCommand::Cancel {
                                        id: cancel_id.clone(),
                                    });
                                    cx.notify();
                                })),
                        )
                    })
                    .when_some(session, |this, session| {
                        this.child(
                            Button::new(format!("auto-run-chat-{}", run.id))
                                .outline()
                                .small()
                                .h_10()
                                .icon(icon(kit_icons::MessagesSquare.1))
                                .label("Open chat")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_session(&session, window, cx)
                                })),
                        )
                    })
                    .when(
                        !run.reviewed
                            && matches!(
                                run.status,
                                RunStatus::Succeeded
                                    | RunStatus::Failed
                                    | RunStatus::Interrupted
                            ),
                        |this| {
                            this.child(
                                Button::new(format!("auto-run-review-{}", run.id))
                                    .ghost()
                                    .small()
                                    .h_10()
                                    .icon(icon(kit_icons::Eye.1))
                                    .label("Mark reviewed")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send_automation(AutomationCommand::Review {
                                            id: review_id.clone(),
                                        });
                                        cx.notify();
                                    })),
                            )
                        },
                    )
                    .child(
                        Button::new(format!("auto-run-delete-{}", run.id))
                            .ghost()
                            .small()
                            .h_10()
                            .icon(icon(kit_icons::Trash.1))
                            .accessibility_label("Delete run")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.auto_run_delete_confirm = Some(delete_id.clone());
                                cx.notify();
                            })),
                    ),
            )
    }

    /// Automation editor overlay — name, prompt, model/effort, a small
    /// schedule preset set, and the worktree/enabled toggles.
    fn render_auto_form(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.auto_editing.clone();
        let is_new = editing.as_ref().is_some_and(|def| def.revision == 0);
        let schedule = self.auto_schedule;
        let entity = cx.entity();
        // `Existing` only appears while it is the active pick — selecting
        // a preset is a one-way door to a schedule the form can express.
        let kinds: Vec<AutoSchedule> = if schedule == AutoSchedule::Existing {
            vec![
                AutoSchedule::Existing,
                AutoSchedule::Manual,
                AutoSchedule::Interval,
                AutoSchedule::DailyUtc,
            ]
        } else {
            vec![
                AutoSchedule::Manual,
                AutoSchedule::Interval,
                AutoSchedule::DailyUtc,
            ]
        };
        let schedule_label = if schedule == AutoSchedule::Existing {
            editing
                .as_ref()
                .map(|def| def.schedule.label())
                .unwrap_or_else(|| schedule.label().to_string())
        } else {
            schedule.label().to_string()
        };
        let project_label = editing
            .as_ref()
            .and_then(|def| def.project.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "No project".into());
        div()
            .id("auto-form")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("auto-form-close")
                            .ghost()
                            .h_11()
                            .icon(icon(kit_icons::X.1))
                            .accessibility_label("Close automation form")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.auto_editing = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .font_bold()
                            .child(if is_new { "New automation" } else { "Edit automation" }),
                    ),
            )
            .child(
                div()
                    .id("auto-form-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("Project: {project_label}")),
                            )
                            .child(Input::new(&self.auto_name).aria_label("Automation name").h_11())
                            .child(Textarea::new(&self.auto_prompt).aria_label("Automation prompt"))
                            .child(
                                div().flex().gap_2().children([
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.auto_model).aria_label("Model").h_11(),
                                    ),
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.auto_effort).aria_label("Effort").h_11(),
                                    ),
                                ]),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("auto-schedule-menu")
                                            .outline()
                                            .h_11()
                                            .label(format!("Schedule: {schedule_label} ▾"))
                                            .accessibility_label("Schedule")
                                            .dropdown_caret(true)
                                            .dropdown_menu_with_anchor(
                                                Anchor::BottomLeft,
                                                move |menu, _, _| {
                                                    kinds.iter().fold(menu, |menu, kind| {
                                                        let entity = entity.clone();
                                                        let kind = *kind;
                                                        menu.item(
                                                            PopupMenuItem::new(
                                                                if kind == AutoSchedule::Existing {
                                                                    schedule_label.clone()
                                                                } else {
                                                                    kind.label().to_string()
                                                                },
                                                            )
                                                            .checked(kind == schedule)
                                                            .on_click(move |_, _, cx| {
                                                                let _ = entity.update(
                                                                    cx,
                                                                    |this, cx| {
                                                                        this.auto_schedule = kind;
                                                                        cx.notify();
                                                                    },
                                                                );
                                                            }),
                                                        )
                                                    })
                                                },
                                            ),
                                    )
                                    .when(schedule == AutoSchedule::Interval, |this| {
                                        this.child(
                                            div().w_24().child(
                                                Input::new(&self.auto_minutes)
                                                    .aria_label("Minutes between runs")
                                                    .h_11(),
                                            ),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("auto-worktree-toggle")
                                            .outline()
                                            .h_11()
                                            .flex_1()
                                            .icon(icon(if self.auto_worktree {
                                                kit_icons::Check.1
                                            } else {
                                                kit_icons::Minus.1
                                            }))
                                            .label("Run in worktree")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.auto_worktree = !this.auto_worktree;
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("auto-enabled-toggle")
                                            .outline()
                                            .h_11()
                                            .flex_1()
                                            .icon(icon(if self.auto_enabled {
                                                kit_icons::Check.1
                                            } else {
                                                kit_icons::Minus.1
                                            }))
                                            .label("Enabled")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.auto_enabled = !this.auto_enabled;
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("auto-form-save")
                            .primary()
                            .h_11()
                            .flex_1()
                            .label(if is_new { "Create automation" } else { "Save" })
                            .on_click(cx.listener(|this, _, _, cx| this.save_auto_form(cx))),
                    ),
            )
    }
}

fn git_status_color(status: char, cx: &App) -> Hsla {
    match status {
        'A' | '?' => cx.theme().success,
        'M' => cx.theme().warning,
        'D' => cx.theme().danger,
        'R' | 'C' => cx.theme().info,
        _ => cx.theme().muted_foreground,
    }
}

impl Render for MobileApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.screen {
            Screen::Connect => self.render_connect(cx).into_any_element(),
            Screen::Main => self.render_main(window, cx).into_any_element(),
        }
    }
}
