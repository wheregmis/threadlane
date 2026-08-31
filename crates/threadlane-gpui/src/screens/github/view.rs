use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::link::Link;
use gpui_component::resizable::{h_resizable, resizable_panel, ResizableState};
use gpui_component::scroll::{ScrollableElement, Scrollbar};
use gpui_component::spinner::Spinner;
use gpui_component::status_bar::StatusBar;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tag::Tag;
use gpui_component::text::{TextView, TextViewState};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt};
use threadlane_git::{
    GitHubIssueDetail, GitHubIssueRef, GitHubIssueSummary, GitHubPrFile, GitHubPrInfo,
    GitHubPullRequestSummary, GitHubRepository, PrCheckStatus,
};

use crate::app::actions::AppAction;
use crate::app::controller;
use crate::state::{AppState, SessionInfo};

actions!(
    threadlane_github,
    [SelectPrevious, SelectNext, OpenSelected]
);

const GITHUB_LIST_CONTEXT: &str = "GitHubList";
const GITHUB_PR_TABS_CONTEXT: &str = "GitHubPullRequestTabs";
const GITHUB_PR_FILE_LIST_CONTEXT: &str = "GitHubPullRequestFiles";
const PAGE_SIZE: usize = 50;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PrDetailTab {
    #[default]
    Summary,
    Timeline,
    Code,
}

impl PrDetailTab {
    const ALL: [Self; 3] = [Self::Summary, Self::Timeline, Self::Code];

    fn label(self) -> &'static str {
        match self {
            Self::Summary => "Summary",
            Self::Timeline => "Timeline",
            Self::Code => "Code",
        }
    }

    fn ix(self) -> usize {
        match self {
            Self::Summary => 0,
            Self::Timeline => 1,
            Self::Code => 2,
        }
    }

    fn adjacent(self, delta: isize) -> Self {
        Self::ALL[self
            .ix()
            .saturating_add_signed(delta)
            .min(Self::ALL.len() - 1)]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PrWorkspaceKey {
    project: PathBuf,
    number: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PrWorkspaceSelection {
    tab: PrDetailTab,
    selected_file: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PrWorkspaceSelections {
    by_pr: HashMap<PrWorkspaceKey, PrWorkspaceSelection>,
}

impl PrWorkspaceSelections {
    fn tab(&self, key: &PrWorkspaceKey) -> PrDetailTab {
        self.by_pr
            .get(key)
            .map(|state| state.tab)
            .unwrap_or_default()
    }

    fn select_tab(&mut self, key: PrWorkspaceKey, tab: PrDetailTab) {
        self.by_pr.entry(key).or_default().tab = tab;
    }

    fn selected_file<'a>(&'a self, key: &PrWorkspaceKey) -> Option<&'a str> {
        self.by_pr
            .get(key)
            .and_then(|state| state.selected_file.as_deref())
    }

    fn select_file(&mut self, key: PrWorkspaceKey, path: String) -> bool {
        let selected = &mut self.by_pr.entry(key).or_default().selected_file;
        if selected.as_deref() == Some(path.as_str()) {
            return false;
        }
        *selected = Some(path);
        true
    }

    fn reconcile_files(&mut self, key: &PrWorkspaceKey, files: &[GitHubPrFile]) {
        let state = self.by_pr.entry(key.clone()).or_default();
        if state
            .selected_file
            .as_ref()
            .is_some_and(|selected| files.iter().any(|file| file.path == *selected))
        {
            return;
        }
        state.selected_file = files.first().map(|file| file.path.clone());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrCommentAttempt {
    token: u64,
    key: PrWorkspaceKey,
    target: PrCommentTarget,
    body: String,
    pr_url: String,
    pre_write_ids: HashSet<String>,
}

const INVALID_PR_REPLY_TARGET: &str =
    "This review comment can’t be replied to because GitHub returned an invalid comment ID.";

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrReplyTarget {
    remote_id: String,
    author: String,
    body: String,
    path: Option<String>,
    line: Option<u64>,
}

impl PrReplyTarget {
    fn comment_id(&self) -> Result<u64, &'static str> {
        self.remote_id
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or(INVALID_PR_REPLY_TARGET)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PrCommentTarget {
    PullRequest,
    Reply(PrReplyTarget, u64),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PrCommentPhase {
    #[default]
    Idle,
    Publishing,
    Checking,
    Present,
    Absent,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrCommentControl {
    Post,
    ClearDraft,
    Retry,
    CheckAgain,
    PostNewDraft,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PrCommentPublish {
    phase: PrCommentPhase,
    attempt: Option<PrCommentAttempt>,
    error: Option<String>,
}

impl PrCommentPublish {
    fn is_active(&self) -> bool {
        matches!(
            self.phase,
            PrCommentPhase::Publishing | PrCommentPhase::Checking
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PrCommentDraft {
    body: String,
    publish: PrCommentPublish,
    reply: Option<PrReplyDraft>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrReplyDraft {
    target: PrReplyTarget,
    body: String,
    publish: PrCommentPublish,
    blocked: bool,
}

fn pr_publish_control(body: &str, publish: &PrCommentPublish) -> PrCommentControl {
    let phase = publish.phase;
    if matches!(
        phase,
        PrCommentPhase::Present | PrCommentPhase::Absent | PrCommentPhase::Unknown
    ) && publish
        .attempt
        .as_ref()
        .is_some_and(|attempt| attempt.body != body)
    {
        return PrCommentControl::PostNewDraft;
    }
    match phase {
        PrCommentPhase::Idle | PrCommentPhase::Publishing | PrCommentPhase::Checking => {
            PrCommentControl::Post
        }
        PrCommentPhase::Present => PrCommentControl::ClearDraft,
        PrCommentPhase::Absent => PrCommentControl::Retry,
        PrCommentPhase::Unknown => PrCommentControl::CheckAgain,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PrCommentDrafts {
    by_pr: HashMap<PrWorkspaceKey, PrCommentDraft>,
    next_attempt_token: u64,
}

impl PrCommentDrafts {
    fn get(&self, key: &PrWorkspaceKey) -> Option<&PrCommentDraft> {
        self.by_pr.get(key)
    }

    fn set_body(&mut self, key: PrWorkspaceKey, body: String) {
        self.by_pr.entry(key).or_default().body = body;
    }

    fn select_reply_target(&mut self, key: PrWorkspaceKey, target: PrReplyTarget) -> bool {
        let draft = self.by_pr.entry(key).or_default();
        if let Some(reply) = draft.reply.as_mut() {
            if reply.target == target {
                reply.blocked = false;
                return true;
            }
            if !reply.body.is_empty() {
                reply.blocked = true;
                return false;
            }
        }
        draft.reply = Some(PrReplyDraft {
            target,
            body: String::new(),
            publish: PrCommentPublish::default(),
            blocked: false,
        });
        true
    }

    fn set_reply_body(&mut self, key: &PrWorkspaceKey, body: String) {
        if let Some(reply) = self
            .by_pr
            .get_mut(key)
            .and_then(|draft| draft.reply.as_mut())
        {
            reply.body = body;
            reply.blocked = false;
        }
    }

    fn begin(
        &mut self,
        key: &PrWorkspaceKey,
        pr_url: String,
        pre_write_ids: HashSet<String>,
    ) -> Option<PrCommentAttempt> {
        let draft = self.by_pr.entry(key.clone()).or_default();
        if draft.publish.is_active() || draft.body.trim().is_empty() {
            return None;
        }
        self.next_attempt_token = self.next_attempt_token.saturating_add(1);
        let attempt = PrCommentAttempt {
            token: self.next_attempt_token,
            key: key.clone(),
            target: PrCommentTarget::PullRequest,
            body: draft.body.clone(),
            pr_url,
            pre_write_ids,
        };
        draft.publish = PrCommentPublish {
            phase: PrCommentPhase::Publishing,
            attempt: Some(attempt.clone()),
            error: None,
        };
        Some(attempt)
    }

    fn begin_reply(
        &mut self,
        key: &PrWorkspaceKey,
        pr_url: String,
        pre_write_ids: HashSet<String>,
    ) -> Result<Option<PrCommentAttempt>, &'static str> {
        let Some(reply) = self.by_pr.get(key).and_then(|draft| draft.reply.as_ref()) else {
            return Ok(None);
        };
        let comment_id = reply.target.comment_id()?;
        if reply.publish.is_active() || reply.body.trim().is_empty() {
            return Ok(None);
        }
        let target = reply.target.clone();
        let body = reply.body.clone();
        self.next_attempt_token = self.next_attempt_token.saturating_add(1);
        let attempt = PrCommentAttempt {
            token: self.next_attempt_token,
            key: key.clone(),
            target: PrCommentTarget::Reply(target, comment_id),
            body,
            pr_url,
            pre_write_ids,
        };
        self.by_pr
            .get_mut(key)
            .and_then(|draft| draft.reply.as_mut())
            .expect("reply draft exists")
            .publish = PrCommentPublish {
            phase: PrCommentPhase::Publishing,
            attempt: Some(attempt.clone()),
            error: None,
        };
        Ok(Some(attempt))
    }

    fn matching_publish_mut(
        &mut self,
        attempt: &PrCommentAttempt,
    ) -> Option<&mut PrCommentPublish> {
        let draft = self.by_pr.get_mut(&attempt.key)?;
        let publish = match &attempt.target {
            PrCommentTarget::PullRequest => &mut draft.publish,
            PrCommentTarget::Reply(target, _) => {
                let reply = draft.reply.as_mut()?;
                if reply.target != *target {
                    return None;
                }
                &mut reply.publish
            }
        };
        publish
            .attempt
            .as_ref()
            .is_some_and(|current| current.token == attempt.token)
            .then_some(publish)
    }

    fn mark_checking(&mut self, attempt: &PrCommentAttempt, error: String) -> bool {
        let Some(publish) = self.matching_publish_mut(attempt) else {
            return false;
        };
        publish.phase = PrCommentPhase::Checking;
        publish.error = Some(error);
        true
    }

    fn complete_success(&mut self, attempt: &PrCommentAttempt) -> bool {
        let Some(draft) = self.by_pr.get_mut(&attempt.key) else {
            return false;
        };
        match &attempt.target {
            PrCommentTarget::PullRequest => {
                if !draft
                    .publish
                    .attempt
                    .as_ref()
                    .is_some_and(|current| current.token == attempt.token)
                {
                    return false;
                }
                if draft.body == attempt.body {
                    draft.body.clear();
                }
                draft.publish = PrCommentPublish::default();
            }
            PrCommentTarget::Reply(target, _) => {
                let Some(reply) = draft.reply.as_mut() else {
                    return false;
                };
                if reply.target != *target
                    || !reply
                        .publish
                        .attempt
                        .as_ref()
                        .is_some_and(|current| current.token == attempt.token)
                {
                    return false;
                }
                if reply.body == attempt.body {
                    draft.reply = None;
                } else {
                    reply.publish = PrCommentPublish::default();
                }
            }
        }
        true
    }

    fn begin_recheck(&mut self, key: &PrWorkspaceKey) -> Option<PrCommentAttempt> {
        self.begin_recheck_for(key, false)
    }

    fn begin_reply_recheck(&mut self, key: &PrWorkspaceKey) -> Option<PrCommentAttempt> {
        self.begin_recheck_for(key, true)
    }

    fn begin_recheck_for(&mut self, key: &PrWorkspaceKey, reply: bool) -> Option<PrCommentAttempt> {
        let publish = if reply {
            &self.by_pr.get(key)?.reply.as_ref()?.publish
        } else {
            &self.by_pr.get(key)?.publish
        };
        if publish.phase != PrCommentPhase::Unknown {
            return None;
        }
        let mut attempt = publish.attempt.clone()?;
        self.next_attempt_token = self.next_attempt_token.saturating_add(1);
        attempt.token = self.next_attempt_token;
        let publish = if reply {
            &mut self.by_pr.get_mut(key)?.reply.as_mut()?.publish
        } else {
            &mut self.by_pr.get_mut(key)?.publish
        };
        publish.phase = PrCommentPhase::Checking;
        publish.attempt = Some(attempt.clone());
        Some(attempt)
    }

    fn complete_readback(
        &mut self,
        attempt: &PrCommentAttempt,
        outcome: PrReadback,
        error: String,
    ) -> bool {
        let Some(publish) = self.matching_publish_mut(attempt) else {
            return false;
        };
        publish.phase = match outcome {
            PrReadback::Present => PrCommentPhase::Present,
            PrReadback::Absent => PrCommentPhase::Absent,
            PrReadback::Unknown => PrCommentPhase::Unknown,
        };
        publish.error = Some(error);
        true
    }

    fn clear(&mut self, key: &PrWorkspaceKey) {
        let draft = self.by_pr.entry(key.clone()).or_default();
        draft.body.clear();
        draft.publish = PrCommentPublish::default();
    }

    fn clear_reply(&mut self, key: &PrWorkspaceKey) {
        self.by_pr.entry(key.clone()).or_default().reply = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrReadback {
    Present,
    Absent,
    Unknown,
}

fn normalized_pr_body(body: &str) -> String {
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn classify_pr_readback(
    attempt: &PrCommentAttempt,
    detail: Result<&GitHubPrInfo, &str>,
) -> PrReadback {
    let Ok(detail) = detail else {
        return PrReadback::Unknown;
    };
    if detail.number != attempt.key.number {
        return PrReadback::Unknown;
    }
    let expected = normalized_pr_body(&attempt.body);
    let present = match attempt.target {
        PrCommentTarget::PullRequest => detail.issue_comments.iter().any(|comment| {
            !attempt.pre_write_ids.contains(&comment.remote_id)
                && normalized_pr_body(&comment.body) == expected
        }),
        PrCommentTarget::Reply(..) => detail.review_comments.iter().any(|comment| {
            !attempt.pre_write_ids.contains(&comment.remote_id)
                && normalized_pr_body(&comment.body) == expected
        }),
    };
    if present {
        PrReadback::Present
    } else {
        PrReadback::Absent
    }
}

fn pr_publish_refresh_matches_selection(
    attempt: &PrCommentAttempt,
    selected: Option<&PrWorkspaceKey>,
) -> bool {
    selected == Some(&attempt.key)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrTimelineKind {
    IssueComment,
    Review,
    InlineReviewComment,
}

impl PrTimelineKind {
    fn label(self) -> &'static str {
        match self {
            Self::IssueComment => "Comment",
            Self::Review => "Review",
            Self::InlineReviewComment => "Inline comment",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrTimelineRow {
    remote_id: String,
    kind: PrTimelineKind,
    author: String,
    body: String,
    timestamp: String,
    url: String,
    review_state: Option<String>,
    path: Option<String>,
    line: Option<u64>,
}

impl PrTimelineRow {
    fn label(&self) -> String {
        match self.kind {
            PrTimelineKind::IssueComment => "Comment".into(),
            PrTimelineKind::Review
                if self
                    .review_state
                    .as_deref()
                    .is_some_and(|state| state.eq_ignore_ascii_case("APPROVED")) =>
            {
                "Approved".into()
            }
            PrTimelineKind::Review
                if self
                    .review_state
                    .as_deref()
                    .is_some_and(|state| state.eq_ignore_ascii_case("CHANGES_REQUESTED")) =>
            {
                "Changes requested".into()
            }
            PrTimelineKind::Review => self
                .review_state
                .as_deref()
                .and_then(review_state_label)
                .unwrap_or_else(|| "Review".into()),
            PrTimelineKind::InlineReviewComment => "Inline comment".into(),
        }
    }

    fn location(&self) -> Option<String> {
        self.path.as_ref().map(|path| match self.line {
            Some(line) => format!("{path}:{line}"),
            None => path.clone(),
        })
    }

    fn reply_target(&self) -> Option<PrReplyTarget> {
        (self.kind == PrTimelineKind::InlineReviewComment).then(|| PrReplyTarget {
            remote_id: self.remote_id.clone(),
            author: self.author.clone(),
            body: self.body.clone(),
            path: self.path.clone(),
            line: self.line,
        })
    }
}

fn review_state_label(state: &str) -> Option<String> {
    let normalized = state.trim().replace('_', " ").to_ascii_lowercase();
    let mut characters = normalized.chars();
    let first = characters.next()?;
    Some(first.to_uppercase().chain(characters).collect())
}

fn merge_pr_timeline(pr: &GitHubPrInfo) -> Vec<PrTimelineRow> {
    let mut rows = pr
        .issue_comments
        .iter()
        .map(|comment| PrTimelineRow {
            remote_id: comment.remote_id.clone(),
            kind: PrTimelineKind::IssueComment,
            author: comment.author.clone(),
            body: comment.body.clone(),
            timestamp: comment.created_at.clone(),
            url: if comment.url.is_empty() {
                pr.url.clone()
            } else {
                comment.url.clone()
            },
            review_state: None,
            path: None,
            line: None,
        })
        .chain(pr.reviews.iter().map(|review| PrTimelineRow {
            remote_id: review.remote_id.clone(),
            kind: PrTimelineKind::Review,
            author: review.author.clone(),
            body: review.body.clone(),
            timestamp: review.submitted_at.clone(),
            url: pr.url.clone(),
            review_state: Some(review.state.clone()),
            path: None,
            line: None,
        }))
        .chain(pr.review_comments.iter().map(|comment| PrTimelineRow {
            remote_id: comment.remote_id.clone(),
            kind: PrTimelineKind::InlineReviewComment,
            author: comment.author.clone(),
            body: comment.body.clone(),
            timestamp: comment.created_at.clone(),
            url: pr.url.clone(),
            review_state: None,
            path: comment.path.clone(),
            line: comment.line,
        }))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
    rows
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrDiffRequest {
    key: PrWorkspaceKey,
    path: String,
    revision: u64,
}

fn pr_diff_result_matches_request(
    result: &PrDiffRequest,
    active: Option<&PrDiffRequest>,
    selected_key: Option<&PrWorkspaceKey>,
    selected_path: Option<&str>,
) -> bool {
    active == Some(result)
        && selected_key == Some(&result.key)
        && selected_path == Some(result.path.as_str())
}

fn decode_git_quoted_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] != b'\\' {
            decoded.push(bytes[ix]);
            ix += 1;
            continue;
        }
        ix += 1;
        let escaped = *bytes.get(ix)?;
        if matches!(escaped, b'0'..=b'7') {
            let mut value = 0u16;
            let mut digits = 0;
            while digits < 3 && ix < bytes.len() && matches!(bytes[ix], b'0'..=b'7') {
                value = value * 8 + u16::from(bytes[ix] - b'0');
                ix += 1;
                digits += 1;
            }
            decoded.push(u8::try_from(value).ok()?);
            continue;
        }
        decoded.push(match escaped {
            b'a' => 7,
            b'b' => 8,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 11,
            b'f' => 12,
            b'r' => b'\r',
            other => other,
        });
        ix += 1;
    }
    String::from_utf8(decoded).ok()
}

fn diff_header_matches_path(header: &str, path: &str) -> bool {
    let Some(rest) = header.strip_prefix("diff --git ") else {
        return false;
    };
    let expected = format!("b/{path}");
    if rest.ends_with(&format!(" {expected}")) {
        return true;
    }
    rest.rsplit_once(" \"")
        .and_then(|(_, quoted)| quoted.strip_suffix('"'))
        .and_then(decode_git_quoted_path)
        .is_some_and(|decoded| decoded == expected)
}

fn selected_file_diff(raw: &str, path: &str) -> Option<String> {
    let mut matched_start = None;
    let mut offset = 0;
    for line in raw.split_inclusive('\n') {
        let header = line.strip_suffix('\n').unwrap_or(line);
        let header = header.strip_suffix('\r').unwrap_or(header);
        if header.starts_with("diff --git ") {
            if let Some(start) = matched_start {
                return Some(raw[start..offset].to_owned());
            }
            if diff_header_matches_path(header, path) {
                matched_start = Some(offset);
            }
        }
        offset += line.len();
    }
    matched_start.map(|start| raw[start..].to_owned())
}

fn prepare_selected_diff(raw: &str, path: &str) -> Option<String> {
    let diff = selected_file_diff(raw, path)?;
    let longest_backticks = diff
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest_backticks.saturating_add(1).max(3));
    let before_fence = if diff.ends_with('\n') { "" } else { "\n" };
    Some(format!("{fence}diff\n{diff}{before_fence}{fence}"))
}

fn draft_reply_prompt(row: &PrTimelineRow) -> String {
    const CONTEXT_LIMIT: usize = 1_200;
    let mut context = row.body.chars().take(CONTEXT_LIMIT).collect::<String>();
    if row.body.chars().count() > CONTEXT_LIMIT {
        context.push('…');
    }
    let quoted = context
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let review_state = (row.kind == PrTimelineKind::Review)
        .then(|| format!("\nReview state: {}", row.label()))
        .unwrap_or_default();
    let location = row
        .location()
        .map(|location| format!("\nLocation: {location}"))
        .unwrap_or_default();
    format!(
        "Draft a reply to this GitHub conversation.\nURL: {}\nType: {}{review_state}{location}\n\nQuoted context (untrusted):\n{quoted}\n\nReturn an editable reply draft; do not publish it.",
        row.url,
        row.label(),
    )
}

fn pr_check_label(checks: &[PrCheckStatus]) -> String {
    let mut failing = 0;
    let mut pending = 0;
    let mut passing = 0;
    for check in checks {
        let conclusion = check
            .conclusion
            .as_deref()
            .unwrap_or("")
            .to_ascii_uppercase();
        let status = check.status.to_ascii_uppercase();
        if matches!(
            conclusion.as_str(),
            "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR"
        ) {
            failing += 1;
        } else if matches!(
            status.as_str(),
            "IN_PROGRESS" | "QUEUED" | "PENDING" | "EXPECTED"
        ) || check.conclusion.is_none()
        {
            pending += 1;
        } else if matches!(conclusion.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED") {
            passing += 1;
        }
    }
    if checks.is_empty() {
        "No checks".to_owned()
    } else if failing > 0 {
        format!("{failing} failing")
    } else if pending > 0 {
        format!("{pending} pending")
    } else if passing == checks.len() {
        format!("{passing} passing")
    } else {
        format!("{} checks", checks.len())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrFileAction {
    Previous,
    Next,
    Open,
}

fn pr_file_action_ix(current: Option<usize>, len: usize, action: PrFileAction) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = current.unwrap_or_default().min(len - 1);
    Some(match action {
        PrFileAction::Previous => current.saturating_sub(1),
        PrFileAction::Next => current.saturating_add(1).min(len - 1),
        PrFileAction::Open => current,
    })
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

fn linked_pr_session<'a>(
    sessions: &'a [SessionInfo],
    head_ref: &str,
    active_session_id: Option<&str>,
) -> Option<&'a SessionInfo> {
    sessions
        .iter()
        .filter(|session| session.git_branch.as_deref() == Some(head_ref))
        .find(|session| active_session_id == Some(session.id.as_str()))
        .or_else(|| {
            sessions
                .iter()
                .find(|session| session.git_branch.as_deref() == Some(head_ref))
        })
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

fn issue_start_dialog_result(result: Result<bool, String>, on_error: impl FnOnce(String)) -> bool {
    match result {
        Ok(started) => started,
        Err(error) => {
            on_error(error);
            false
        }
    }
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
        issue_start_dialog_result(result, |error| {
            self.error = Some(error);
            cx.notify();
        })
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
    pr_timeline_rows: Vec<PrTimelineRow>,
    pr_timeline_list_state: ListState,
    pr_file_list_state: ListState,
    pr_diff_body: Entity<TextViewState>,
    pr_selections: PrWorkspaceSelections,
    pr_drafts: PrCommentDrafts,
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
            pr_timeline_rows: Vec::new(),
            pr_timeline_list_state: ListState::new(0, ListAlignment::Top, px(112.0)),
            pr_file_list_state: ListState::new(0, ListAlignment::Top, px(52.0)),
            pr_diff_body,
            pr_selections: PrWorkspaceSelections::default(),
            pr_drafts: PrCommentDrafts::default(),
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
            active_detail_request: None,
            issue_list_state: ListState::new(0, ListAlignment::Top, px(88.0)),
            pr_list_state: ListState::new(0, ListAlignment::Top, px(78.0)),
            detail_split_state,
            list_focus: cx.focus_handle(),
            pr_tabs_focus: cx.focus_handle(),
            pr_file_focus: cx.focus_handle(),
            debounce_task: None,
            issue_comment_draft: String::new(),
            pr_review_draft: String::new(),
            linked_sessions_fingerprint,
            _subscriptions: vec![
                model_subscription,
                input_subscription,
                comment_draft_subscription,
                reply_draft_subscription,
            ],
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
                        let key = PrWorkspaceKey {
                            project: request.work_dir.clone(),
                            number: detail.number,
                        };
                        let previous_file =
                            this.pr_selections.selected_file(&key).map(str::to_owned);
                        this.pr_timeline_rows = merge_pr_timeline(&detail);
                        this.pr_timeline_list_state
                            .reset(this.pr_timeline_rows.len());
                        this.pr_selections.reconcile_files(&key, &detail.files);
                        if previous_file.as_deref() != this.pr_selections.selected_file(&key) {
                            this.reset_pr_diff_body(cx);
                        }
                        this.pr_file_list_state.reset(detail.files.len());
                        this.detail_body
                            .update(cx, |body, cx| body.set_text(&detail.body, cx));
                        this.pr_detail = Some(detail);
                        if this.pr_selections.tab(&key) == PrDetailTab::Code {
                            this.load_selected_diff(cx);
                        }
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

    fn current_pr_key(&self) -> Option<PrWorkspaceKey> {
        if self.tab != GitHubTab::PullRequests {
            return None;
        }
        Some(PrWorkspaceKey {
            project: self.project_work_dir.clone()?,
            number: self.selected_pr?,
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
                        this.pr_timeline_list_state
                            .reset(this.pr_timeline_rows.len());
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

    fn linked_pr_task(&self, head_ref: &str, cx: &App) -> Option<SessionInfo> {
        let state = self.model.read(cx);
        let project = state
            .projects
            .iter()
            .find(|project| Some(&project.work_dir) == self.project_work_dir.as_ref())?;
        linked_pr_session(
            &project.sessions,
            head_ref,
            state.active_session_id.as_deref(),
        )
        .cloned()
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
        let linked_task = self.linked_pr_task(&pr.head_ref, cx);
        let checks_label = pr_check_label(&pr.checks);
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
                                        "#{number} · {} · {} · {} → {}",
                                        pr.author, pr.updated_at, pr.head_ref, pr.base_ref,
                                    )),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(Tag::new().small().child(pr.state.clone()))
                                    .children(
                                        pr.is_draft.then(|| Tag::new().small().child("Draft")),
                                    )
                                    .child(Tag::new().small().child(checks_label))
                                    .children(
                                        pr.review_decision
                                            .clone()
                                            .map(|decision| Tag::new().small().child(decision)),
                                    )
                                    .children(linked_task.map(|session| {
                                        Tag::info()
                                            .small()
                                            .child(format!("Task · {}", session.title))
                                    })),
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

    fn handoff_pr_reply(&self, head_ref: &str, prompt: String, cx: &mut Context<Self>) {
        let model = self.model.clone();
        let project_work_dir = self.project_work_dir.clone();
        let head_ref = head_ref.to_owned();
        model.update(cx, |state, cx| {
            let target = project_work_dir
                .as_ref()
                .and_then(|work_dir| {
                    state
                        .projects
                        .iter()
                        .find(|project| project.work_dir == *work_dir)
                })
                .and_then(|project| {
                    linked_pr_session(
                        &project.sessions,
                        &head_ref,
                        state.active_session_id.as_deref(),
                    )
                })
                .map(|session| (session.work_dir.clone(), session.id.clone()));
            if let Some((work_dir, session_id)) = target {
                controller::dispatch(
                    state,
                    AppAction::SelectSession {
                        work_dir,
                        session_id: session_id.clone(),
                    },
                );
                if state.active_session_id.as_deref() != Some(session_id.as_str()) {
                    state.session_status =
                        Some("Couldn’t select the linked task for this pull request.".into());
                    cx.notify();
                    return;
                }
            }
            state.request_composer_prompt(prompt);
            controller::dispatch(state, AppAction::CloseGitHub);
            cx.notify();
        });
    }

    fn render_pr_comment_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.render_pr_conversation_editor(false, cx).unwrap()
    }

    fn render_pr_reply_editor(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.render_pr_conversation_editor(true, cx)
    }

    fn render_pr_conversation_editor(
        &mut self,
        reply: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme().colors;
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
            PrCommentPhase::Present if reply => Some("A matching new review comment was found, but GitHub cannot confirm its reply relationship. Draft retained."),
            PrCommentPhase::Present => Some("A matching new GitHub comment was found for the submitted draft. Draft retained."),
            PrCommentPhase::Absent => Some("No matching new GitHub comment was found for the submitted draft. Draft retained."),
            PrCommentPhase::Unknown => Some("GitHub could not confirm whether the submitted draft was published. Draft retained."),
        };
        let action = match control {
            PrCommentControl::Post | PrCommentControl::Retry | PrCommentControl::PostNewDraft => {
                Button::new(if reply {
                    "github-pr-reply-publish"
                } else {
                    "github-pr-comment-publish"
                })
                .label(match control {
                    PrCommentControl::Retry => "Retry",
                    PrCommentControl::PostNewDraft if reply => "Post new reply",
                    PrCommentControl::PostNewDraft => "Post new draft",
                    _ if reply => "Reply",
                    _ => "Post comment",
                })
                .small()
                .disabled(publish.is_active() || body.trim().is_empty() || invalid_target.is_some())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if reply {
                        this.publish_pr_reply(cx)
                    } else {
                        this.publish_pr_comment(cx)
                    }
                }))
                .into_any_element()
            }
            PrCommentControl::CheckAgain => Button::new(if reply {
                "github-pr-reply-check-again"
            } else {
                "github-pr-comment-check-again"
            })
            .label("Check again")
            .small()
            .on_click(cx.listener(move |this, _, _, cx| {
                if reply {
                    this.check_pr_reply_again(cx)
                } else {
                    this.check_pr_comment_again(cx)
                }
            }))
            .into_any_element(),
            PrCommentControl::ClearDraft => Button::new(if reply {
                "github-pr-reply-clear"
            } else {
                "github-pr-comment-clear"
            })
            .label(if reply {
                "Clear target and draft"
            } else {
                "Clear draft"
            })
            .ghost()
            .small()
            .on_click(cx.listener(move |this, _, window, cx| {
                if reply {
                    this.clear_pr_reply(window, cx)
                } else {
                    this.clear_pr_draft(window, cx)
                }
            }))
            .into_any_element(),
        };
        Some(
            div()
                .flex_none()
                .p_3()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Draft · Not published"),
                )
                .children(target.map(|target| {
                    div()
                        .mt_1()
                        .text_xs()
                        .whitespace_normal()
                        .child(format!("Reply to {}", target.author))
                        .children(target.path.as_ref().map(|path| {
                            format!(
                                " · {path}{}",
                                target
                                    .line
                                    .map(|line| format!(":{line}"))
                                    .unwrap_or_default()
                            )
                        }))
                        .child(
                            div()
                                .mt_1()
                                .text_color(theme.muted_foreground)
                                .child(target.body),
                        )
                }))
                .child(div().mt_2().child(Textarea::new(if reply {
                    &self.pr_reply_input
                } else {
                    &self.pr_comment_input
                })))
                .children(
                    blocked
                        .then_some(
                            "Post or clear this reply draft before replying to another comment.",
                        )
                        .into_iter()
                        .chain(invalid_target)
                        .map(|message| {
                            div()
                                .mt_2()
                                .text_xs()
                                .text_color(theme.danger)
                                .child(message)
                        }),
                )
                .children(status.map(|status| {
                    div()
                        .mt_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(
                            if matches!(
                                publish.phase,
                                PrCommentPhase::Absent | PrCommentPhase::Unknown
                            ) {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            },
                        )
                        .children(publish.is_active().then(|| Spinner::new().xsmall()))
                        .child(status)
                }))
                .children(publish.error.map(|error| {
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(theme.danger)
                        .whitespace_normal()
                        .child(error)
                }))
                .child(
                    div()
                        .mt_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .children(
                            publish
                                .attempt
                                .as_ref()
                                .filter(|_| publish.phase == PrCommentPhase::Present)
                                .map(|attempt| {
                                    Link::new("github-pr-comment-open-present")
                                        .href(attempt.pr_url.clone())
                                        .child("Open on GitHub")
                                }),
                        )
                        .child(action),
                )
                .into_any_element(),
        )
    }

    fn render_pr_timeline_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.pr_timeline_rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let Some(pr) = self.pr_detail.as_ref() else {
            return div().into_any_element();
        };
        let prompt = draft_reply_prompt(&row);
        let head_ref = pr.head_ref.clone();
        let location = row.location();
        let reply_target = row.reply_target();
        let theme = cx.theme().colors;
        div()
            .id(SharedString::from(format!(
                "github-pr-timeline-{}-{}",
                row.kind.label(),
                row.remote_id
            )))
            .p_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(Tag::new().small().child(row.label()))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(row.author.clone()),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(row.timestamp.clone()),
                    )
                    .children(location.map(|location| Tag::new().small().child(location)))
                    .child(div().flex_1())
                    .children(reply_target.map(|target| {
                        Button::new(SharedString::from(format!(
                            "reply-pr-review-{}",
                            target.remote_id
                        )))
                        .label("Reply")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.select_pr_reply_target(target.clone(), window, cx)
                            },
                        ))
                    }))
                    .child(
                        Button::new(SharedString::from(format!(
                            "draft-pr-reply-{}-{}",
                            row.kind.label(),
                            row.remote_id
                        )))
                        .label("Ask agent to draft reply")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.handoff_pr_reply(&head_ref, prompt.clone(), cx);
                        })),
                    ),
            )
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    .whitespace_normal()
                    .child(if row.body.is_empty() {
                        "No review body".to_owned()
                    } else {
                        row.body
                    }),
            )
            .into_any_element()
    }

    fn render_pr_file_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(file) = self
            .pr_detail
            .as_ref()
            .and_then(|detail| detail.files.get(ix))
            .cloned()
        else {
            return div().into_any_element();
        };
        let selected = self.current_pr_file() == Some(file.path.as_str());
        let path = file.path.clone();
        let theme = cx.theme().colors;
        div()
            .id(SharedString::from(format!("github-pr-file-{}", file.path)))
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(if selected {
                theme.list_active
            } else {
                theme.background
            })
            .hover(|style| style.bg(theme.list_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.pr_file_focus.focus(window, cx);
                this.select_pr_file(path.clone(), cx);
            }))
            .child(div().text_sm().truncate().child(file.path))
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "+{} −{} · {}",
                        file.additions, file.deletions, file.change_type
                    )),
            )
            .into_any_element()
    }

    fn render_pr_summary(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(detail) = self
            .pr_detail
            .as_ref()
            .filter(|detail| self.selected_pr == Some(detail.number))
        else {
            return self.render_empty("Loading details…", cx);
        };
        let theme = cx.theme().colors;
        let linked = self.linked_pr_task(&detail.head_ref, cx);
        div()
            .size_full()
            .overflow_y_scrollbar()
            .px_5()
            .py_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(detail.is_draft.then(|| Tag::new().small().child("Draft")))
                    .child(Tag::new().small().child(detail.state.clone()))
                    .children(
                        detail
                            .review_decision
                            .clone()
                            .map(|decision| Tag::new().small().child(decision)),
                    ),
            )
            .child(
                div()
                    .mt_3()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Checks"),
            )
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if detail.total_checks == 0 {
                        "No checks reported".to_owned()
                    } else {
                        format!(
                            "{} passing · {} pending · {} failing",
                            detail.passing_checks, detail.pending_checks, detail.failing_checks
                        )
                    }),
            )
            .child(
                div()
                    .mt_5()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Description"),
            )
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    .child(TextView::new(&self.detail_body).selectable(true)),
            )
            .children(linked.map(|session| {
                let model = self.model.clone();
                let work_dir = session.work_dir.clone();
                let session_id = session.id.clone();
                div()
                    .mt_5()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Linked task"),
                    )
                    .child(Tag::info().small().child(session.title))
                    .child(
                        Button::new("github-open-linked-pr-task")
                            .label("Open task")
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectSession {
                                            work_dir: work_dir.clone(),
                                            session_id: session_id.clone(),
                                        },
                                    );
                                    controller::dispatch(state, AppAction::CloseGitHub);
                                    cx.notify();
                                });
                            }),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn render_pr_timeline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_pr_comment_editor(cx))
            .children(self.render_pr_reply_editor(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .children(self.pr_timeline_rows.is_empty().then(|| {
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No pull-request conversation yet.")
                    }))
                    .children((!self.pr_timeline_rows.is_empty()).then(|| {
                        list(
                            self.pr_timeline_list_state.clone(),
                            cx.processor(Self::render_pr_timeline_row),
                        )
                        .size_full()
                        .with_sizing_behavior(ListSizingBehavior::Infer)
                    }))
                    .children((!self.pr_timeline_rows.is_empty()).then(|| {
                        div()
                            .absolute()
                            .inset_0()
                            .child(Scrollbar::vertical(&self.pr_timeline_list_state))
                    })),
            )
            .into_any_element()
    }

    fn render_pr_code(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let file_count = self
            .pr_detail
            .as_ref()
            .map(|detail| detail.files.len())
            .unwrap_or_default();
        if file_count == 0 {
            return self.render_empty("No changed files reported.", cx);
        }
        let diff_status = if self.diff_loading {
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().xsmall())
                    .child("Loading diff…")
                    .into_any_element(),
            )
        } else {
            self.diff_error.as_ref().map(|error| {
                div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(error.clone())
                    .into_any_element()
            })
        };
        div()
            .size_full()
            .min_h_0()
            .flex()
            .child(
                div()
                    .relative()
                    .w_64()
                    .min_h_0()
                    .border_1()
                    .border_color(theme.border)
                    .focus(|style| style.border_color(theme.primary))
                    .track_focus(&self.pr_file_focus)
                    .key_context(GITHUB_PR_FILE_LIST_CONTEXT)
                    .on_action(cx.listener(Self::select_previous_pr_file))
                    .on_action(cx.listener(Self::select_next_pr_file))
                    .on_action(cx.listener(Self::open_selected_pr_file))
                    .child(
                        list(
                            self.pr_file_list_state.clone(),
                            cx.processor(|this, ix, _window, cx| this.render_pr_file_row(ix, cx)),
                        )
                        .size_full()
                        .with_sizing_behavior(ListSizingBehavior::Infer),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .child(Scrollbar::vertical(&self.pr_file_list_state)),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .min_h_0()
                    .flex_1()
                    .children(diff_status)
                    .children(self.diff_error.is_none().then(|| {
                        TextView::new(&self.pr_diff_body)
                            .selectable(true)
                            .scrollable(true)
                            .size_full()
                            .p_4()
                    })),
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
        let theme = cx.theme().colors;
        let (number, title, url, state, author, head_ref, base_ref, updated_at) = {
            let Some(detail) = self
                .pr_detail
                .as_ref()
                .filter(|detail| self.selected_pr == Some(detail.number))
            else {
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
        let tab = self.current_pr_tab();
        let tabs_focus = self.pr_tabs_focus.clone();
        let tabs = TabBar::new("github-pr-detail-tabs")
            .underline()
            .small()
            .selected_index(tab.ix())
            .children(
                PrDetailTab::ALL
                    .into_iter()
                    .map(|candidate| Tab::new().label(candidate.label())),
            )
            .on_click(cx.listener(move |this, ix, window, cx| {
                tabs_focus.focus(window, cx);
                if let Some(candidate) = PrDetailTab::ALL.get(*ix).copied() {
                    this.select_pr_tab(candidate, cx);
                }
            }));
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_5()
                    .pt_4()
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
                            .child(format!(
                                "#{} · {} · {} · {} → {} · {}",
                                number, state, author, head_ref, base_ref, updated_at
                            )),
                    )
                    .child(
                        div()
                            .mt_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Link::new("github-open-pr-browser")
                                    .href(url)
                                    .child("Open on GitHub"),
                            )
                            .child(
                                div()
                                    .id("github-pr-detail-tabs-focus")
                                    .border_1()
                                    .border_color(theme.background)
                                    .focus(|style| style.border_color(theme.primary))
                                    .track_focus(&self.pr_tabs_focus)
                                    .key_context(GITHUB_PR_TABS_CONTEXT)
                                    .on_action(cx.listener(Self::select_previous_pr_tab))
                                    .on_action(cx.listener(Self::select_next_pr_tab))
                                    .child(tabs),
                            ),
                    )
                    .child(div().mt_3().border_b_1().border_color(theme.border)),
            )
            .child(div().flex_1().min_h_0().child(match tab {
                PrDetailTab::Summary => self.render_pr_summary(cx),
                PrDetailTab::Timeline => self.render_pr_timeline(cx),
                PrDetailTab::Code => self.render_pr_code(cx),
            }))
            .into_any_element()
    }

    fn render_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        if self.selected_ix().is_none() {
            return self.render_empty("Select an item to see details.", cx);
        }
        if let Some(error) = &self.detail_error {
            return self.render_empty(error, cx);
        }
        if self.tab == GitHubTab::PullRequests {
            return self.render_pr_detail(window, cx);
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
        let detail = self.render_detail(window, cx);
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
        detail_result_matches_list, draft_reply_prompt, github_link_fingerprint_rows,
        github_query_mode, github_result_matches_request, github_server_query,
        issue_filter_matches, issue_start_activation, issue_start_confirmation,
        issue_start_dialog_result, linked_pr_session, linked_session_fingerprint,
        linked_session_ids, linked_session_status, linked_sessions_across_projects,
        list_count_splice, merge_pr_timeline, pr_check_label, pr_diff_result_matches_request,
        pr_file_action_ix, pr_publish_control, pr_publish_refresh_matches_selection,
        prepare_selected_diff, selected_file_diff, selected_issue_after_refresh, GitHubQueryMode,
        GitHubRequest, GitHubTab, GitHubView, PrCommentControl, PrCommentDrafts, PrCommentPhase,
        PrDetailTab, PrDiffRequest, PrFileAction, PrReadback, PrReplyTarget, PrTimelineKind,
        PrWorkspaceKey, PrWorkspaceSelections,
    };
    use crate::state::{AppState, SessionHealth, SessionInfo};
    use gpui::AppContext as _;
    use std::path::PathBuf;
    use threadlane_git::{
        GitHubIssueRef, GitHubIssueSummary, GitHubPrFile, GitHubPrInfo, GitHubPullRequestSummary,
        PrCheckStatus, PrConversationComment, PrReview, PrReviewComment,
    };

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
        view.project_work_dir = Some(project);
        view.tab = GitHubTab::PullRequests;
        view.selected_pr = Some(key.number);
        view.pull_requests = vec![GitHubPullRequestSummary {
            number: key.number,
            title: "Inspect PR".into(),
            ..Default::default()
        }];
        view.pr_detail = Some(GitHubPrInfo {
            number: key.number,
            title: "Inspect PR".into(),
            files: files.clone(),
            ..Default::default()
        });
        view.pr_selections.reconcile_files(&key, &files);
        view.pr_list_state.reset(1);
        view.pr_file_list_state.reset(files.len());
        cx.notify();
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
        };

        assert_eq!(
            row(PrTimelineKind::InlineReviewComment).reply_target(),
            Some(reply_target("101", "alice"))
        );
        assert!(row(PrTimelineKind::IssueComment).reply_target().is_none());
        assert!(row(PrTimelineKind::Review).reply_target().is_none());
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
            view.pr_drafts.set_body(first, " first comment\n".into());
            view.sync_pr_draft_inputs(window, cx);
        });
        assert_eq!(
            view.read_with(cx, |view, cx| view.pr_comment_input.read(cx).value()),
            " first comment\n"
        );
        view.update_in(cx, |view, window, cx| {
            let second = pr_key("/projects/app", 43);
            view.selected_pr = Some(43);
            view.pr_drafts.set_body(second, "second comment".into());
            view.sync_pr_draft_inputs(window, cx);
        });
        assert_eq!(
            view.read_with(cx, |view, cx| view.pr_comment_input.read(cx).value()),
            "second comment"
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

    #[test]
    fn github_pr_tab_arrows_and_file_actions_keep_bounded_selection() {
        assert_eq!(PrDetailTab::Summary.adjacent(1), PrDetailTab::Timeline);
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
        cx.update(|window, cx| {
            window.focus(&tabs_focus, cx);
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("right");
        assert_eq!(
            view.read_with(cx, |view, _| view.current_pr_tab()),
            PrDetailTab::Timeline
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
        });

        assert!(prompt.contains("https://github.com/threadlane/app/pull/42#discussion_r7"));
        assert!(prompt.contains("> context"));
        assert_eq!(prompt.matches(instruction).count(), 1);
        assert!(prompt.chars().count() < 1_700);
    }

    #[test]
    fn github_pr_linked_session_prefers_the_active_matching_branch() {
        let mut first = session("first", None);
        first.git_branch = Some("feature/pr-workspace".into());
        let mut active = session("active", None);
        active.git_branch = Some("feature/pr-workspace".into());
        let mut other = session("other", None);
        other.git_branch = Some("feature/other".into());
        let sessions = vec![first, active, other];

        assert_eq!(
            linked_pr_session(&sessions, "feature/pr-workspace", Some("active"))
                .map(|session| session.id.as_str()),
            Some("active")
        );
        assert_eq!(
            linked_pr_session(&sessions, "feature/pr-workspace", Some("missing"))
                .map(|session| session.id.as_str()),
            Some("first")
        );
        assert!(linked_pr_session(&sessions, "feature/missing", None).is_none());
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
}
