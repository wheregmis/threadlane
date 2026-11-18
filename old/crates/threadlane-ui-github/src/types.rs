use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use gpui_kit::base::Link;
use threadlane_git::{
    GitHubIssueDetail, GitHubIssueSummary, GitHubPrFile, GitHubPrInfo, GitHubPrViewedState,
    GitHubPullRequestSummary, PrFileViewedStatus,
};

use threadlane_ui_state::SessionInfo;
pub use threadlane_ui_state::GitHubTab;

pub const GITHUB_LIST_CONTEXT: &str = "GitHubList";
pub const GITHUB_PR_TABS_CONTEXT: &str = "GitHubPullRequestTabs";
pub const GITHUB_PR_FILE_LIST_CONTEXT: &str = "GitHubPullRequestFiles";
pub const PAGE_SIZE: usize = 50;

pub use threadlane_ui_kit::github::GitHubStateFilter;

pub fn github_state_for_tab(state: GitHubStateFilter, tab: GitHubTab) -> GitHubStateFilter {
    if tab == GitHubTab::Issues && state == GitHubStateFilter::Merged {
        GitHubStateFilter::Open
    } else {
        state
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubScope {
    All,
    Project(PathBuf),
}

impl GitHubScope {
    pub fn label(&self, projects: &[(String, PathBuf)]) -> String {
        match self {
            Self::All => "All projects".into(),
            Self::Project(work_dir) => projects
                .iter()
                .find(|(_, dir)| dir == work_dir)
                .map(|(name, _)| name.clone())
                .or_else(|| {
                    work_dir
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "Unknown project".into()),
        }
    }

    pub fn projects(&self, all: &[(String, PathBuf)]) -> Vec<(String, PathBuf)> {
        match self {
            Self::All => all.to_vec(),
            Self::Project(work_dir) => all
                .iter()
                .find(|(_, dir)| dir == work_dir)
                .cloned()
                .into_iter()
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitHubItemKey {
    pub project: PathBuf,
    pub number: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedIssue {
    pub project: PathBuf,
    pub project_name: String,
    pub summary: GitHubIssueSummary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedPr {
    pub project: PathBuf,
    pub project_name: String,
    pub summary: GitHubPullRequestSummary,
}

fn scoped_order_key(updated_at: &str, project_name: &str, number: u64) -> (String, String, u64) {
    (updated_at.to_owned(), project_name.to_owned(), number)
}

pub fn merge_scoped_issues(mut rows: Vec<ScopedIssue>, limit: usize) -> Vec<ScopedIssue> {
    rows.sort_by(|left, right| {
        scoped_order_key(
            &right.summary.updated_at,
            &right.project_name,
            right.summary.issue.number,
        )
        .cmp(&scoped_order_key(
            &left.summary.updated_at,
            &left.project_name,
            left.summary.issue.number,
        ))
    });
    rows.truncate(limit);
    rows
}

pub fn merge_scoped_prs(mut rows: Vec<ScopedPr>, limit: usize) -> Vec<ScopedPr> {
    rows.sort_by(|left, right| {
        scoped_order_key(
            &right.summary.updated_at,
            &right.project_name,
            right.summary.number,
        )
        .cmp(&scoped_order_key(
            &left.summary.updated_at,
            &left.project_name,
            left.summary.number,
        ))
    });
    rows.truncate(limit);
    rows
}

pub fn selected_scoped_issue_after_refresh(
    selected: Option<GitHubItemKey>,
    rows: &[ScopedIssue],
) -> Option<GitHubItemKey> {
    selected
        .filter(|selected| {
            rows.iter().any(|row| {
                row.project == selected.project && row.summary.issue.number == selected.number
            })
        })
        .or_else(|| {
            rows.first().map(|row| GitHubItemKey {
                project: row.project.clone(),
                number: row.summary.issue.number,
            })
        })
}

pub fn selected_scoped_pr_after_refresh(
    selected: Option<GitHubItemKey>,
    rows: &[ScopedPr],
) -> Option<GitHubItemKey> {
    selected
        .filter(|selected| {
            rows.iter()
                .any(|row| row.project == selected.project && row.summary.number == selected.number)
        })
        .or_else(|| {
            rows.first().map(|row| GitHubItemKey {
                project: row.project.clone(),
                number: row.summary.number,
            })
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRequest {
    pub scope: GitHubScope,
    pub tab: GitHubTab,
    pub query_revision: u64,
    pub item: Option<GitHubItemKey>,
}

pub fn github_result_matches_request(result: &GitHubRequest, current: &GitHubRequest) -> bool {
    result == current
}

pub fn detail_result_matches_list(
    detail: &GitHubRequest,
    list: &GitHubRequest,
    selected: Option<GitHubItemKey>,
) -> bool {
    list.item.is_none()
        && detail.scope == list.scope
        && detail.tab == list.tab
        && detail.query_revision == list.query_revision
        && detail.item == selected
}

pub use threadlane_ui_kit::github::PrDetailTab;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PrWorkspaceKey {
    pub project: PathBuf,
    pub number: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrWorkspaceSelection {
    pub tab: PrDetailTab,
    pub selected_file: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrWorkspaceSelections {
    pub by_pr: HashMap<PrWorkspaceKey, PrWorkspaceSelection>,
}

impl PrWorkspaceSelections {
    pub fn tab(&self, key: &PrWorkspaceKey) -> PrDetailTab {
        self.by_pr
            .get(key)
            .map(|state| state.tab)
            .unwrap_or_default()
    }

    pub fn select_tab(&mut self, key: PrWorkspaceKey, tab: PrDetailTab) {
        self.by_pr.entry(key).or_default().tab = tab;
    }

    pub fn selected_file<'a>(&'a self, key: &PrWorkspaceKey) -> Option<&'a str> {
        self.by_pr
            .get(key)
            .and_then(|state| state.selected_file.as_deref())
    }

    pub fn select_file(&mut self, key: PrWorkspaceKey, path: String) -> bool {
        let selected = &mut self.by_pr.entry(key).or_default().selected_file;
        if selected.as_deref() == Some(path.as_str()) {
            return false;
        }
        *selected = Some(path);
        true
    }

    pub fn reconcile_files(&mut self, key: &PrWorkspaceKey, files: &[GitHubPrFile]) {
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

/// Confirmed snapshot of one signed-in account's Viewed markers for a PR.
/// Markers are keyed by exact path — renames never inherit a basename's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrViewedSnapshot {
    /// GitHub node ID the mark/unmark mutations target.
    pub pull_request_id: String,
    /// Head OID the markers were read against.
    pub head_oid: String,
    /// Account these personal markers belong to.
    pub viewer: String,
    pub files: HashMap<String, PrFileViewedStatus>,
    /// False when pagination stopped before every file was reported.
    pub complete: bool,
}

impl PrViewedSnapshot {
    pub fn status(&self, path: &str) -> PrFileViewedStatus {
        self.files
            .get(path)
            .copied()
            .unwrap_or(PrFileViewedStatus::Unknown)
    }
}

/// One in-flight (or outcome-uncertain) viewed write for a single file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrViewedWrite {
    pub path: String,
    pub viewed: bool,
    pub token: u64,
    /// The transport confirmed the mutation; the write stays pending — not
    /// counted, not rewritable — until its readback settles.
    pub confirmed: bool,
    /// The transport did not confirm the outcome; GitHub may have applied the
    /// write. Another write is blocked until a refresh settles the state.
    pub uncertain: bool,
}

/// Ephemeral viewed-marker lifecycle for one PR workspace. The last confirmed
/// snapshot is retained across refreshes so failures never read as a zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrViewedState {
    pub snapshot: Option<PrViewedSnapshot>,
    pub loading: bool,
    pub error: Option<String>,
    /// Latest head OID observed by the PR detail fetch; writes require the
    /// snapshot to have been read against the same head.
    pub observed_head_oid: String,
    /// Most recent read/write generation; delayed results are dropped unless
    /// their token still matches.
    pub token: u64,
    pub pending_write: Option<PrViewedWrite>,
}

impl PrViewedState {
    /// A file's marker may only be written from a settled, current snapshot.
    pub fn write_allowed(&self, path: &str) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        !self.loading
            && self.pending_write.is_none()
            && !self.observed_head_oid.is_empty()
            && snapshot.head_oid == self.observed_head_oid
            && snapshot.files.contains_key(path)
    }

    /// Marker text for a file row: status first, then write/read phase.
    pub fn marker_label(&self, path: &str) -> Option<&'static str> {
        if let Some(write) = &self.pending_write {
            if write.path == path {
                return Some(if write.uncertain {
                    "Couldn't confirm — refresh to settle"
                } else {
                    "Saving…"
                });
            }
        }
        match self.snapshot.as_ref().map(|snap| snap.status(path)) {
            Some(PrFileViewedStatus::Viewed) => Some("Viewed"),
            Some(PrFileViewedStatus::ChangedSinceViewed) => Some("Changed since viewed"),
            Some(PrFileViewedStatus::Unviewed) | Some(PrFileViewedStatus::Unknown) | None => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PrViewedStates {
    pub by_pr: HashMap<PrWorkspaceKey, PrViewedState>,
    pub next_token: u64,
}

impl PrViewedStates {
    pub fn get(&self, key: &PrWorkspaceKey) -> Option<&PrViewedState> {
        self.by_pr.get(key)
    }

    /// Refreshes are blocked while a write is still in flight — a read racing
    /// an unconfirmed mutation could return pre-write state. Uncertain and
    /// confirmed-but-unverified writes are exactly what a refresh settles.
    pub fn refresh_allowed(&self, key: &PrWorkspaceKey) -> bool {
        !self.by_pr.get(key).is_some_and(|state| {
            state
                .pending_write
                .as_ref()
                .is_some_and(|write| !(write.uncertain || write.confirmed))
        })
    }

    /// Head observed by the latest detail fetch; a mismatch disables writes
    /// until a fresh viewed read catches up.
    pub fn observe_head(&mut self, key: &PrWorkspaceKey, head_oid: &str) {
        self.by_pr.entry(key.clone()).or_default().observed_head_oid = head_oid.to_owned();
    }

    pub fn begin_read(&mut self, key: &PrWorkspaceKey) -> u64 {
        self.next_token = self.next_token.saturating_add(1);
        let token = self.next_token;
        let state = self.by_pr.entry(key.clone()).or_default();
        state.loading = true;
        state.token = token;
        token
    }

    /// A settled read reflects every completed write and re-arms writes.
    pub fn complete_read(
        &mut self,
        key: &PrWorkspaceKey,
        token: u64,
        snapshot: PrViewedSnapshot,
    ) -> bool {
        let Some(state) = self.by_pr.get_mut(key) else {
            return false;
        };
        if state.token != token {
            return false;
        }
        state.loading = false;
        state.error = None;
        state.pending_write = None;
        state.snapshot = Some(snapshot);
        true
    }

    pub fn fail_read(&mut self, key: &PrWorkspaceKey, token: u64, error: String) -> bool {
        let Some(state) = self.by_pr.get_mut(key) else {
            return false;
        };
        if state.token != token || !state.loading {
            return false;
        }
        state.loading = false;
        // A confirmed write whose readback failed is awaiting verification,
        // not rolled back: the marker stays blocked until a refresh settles.
        if let Some(write) = state.pending_write.as_mut() {
            write.uncertain = true;
        }
        state.error = Some(error);
        true
    }

    /// Starting a write bumps the generation so an older in-flight read can no
    /// longer overwrite newer write/readback state.
    pub fn begin_write(&mut self, key: &PrWorkspaceKey, path: String, viewed: bool) -> Option<u64> {
        if !self
            .by_pr
            .get(key)
            .is_some_and(|state| state.write_allowed(&path))
        {
            return None;
        }
        self.next_token = self.next_token.saturating_add(1);
        let token = self.next_token;
        let state = self.by_pr.entry(key.clone()).or_default();
        state.token = token;
        state.error = None;
        state.pending_write = Some(PrViewedWrite {
            path,
            viewed,
            token,
            confirmed: false,
            uncertain: false,
        });
        Some(token)
    }

    /// Result of a write. Ok means GitHub confirmed; Err may mean the write
    /// landed anyway, so the file stays blocked until a read settles it.
    /// Returns true when the token still matches (caller refreshes on Ok).
    pub fn complete_write(&mut self, key: &PrWorkspaceKey, token: u64, result: Result<(), String>) -> bool {
        let Some(state) = self.by_pr.get_mut(key) else {
            return false;
        };
        let Some(write) = state.pending_write.as_mut() else {
            return false;
        };
        if write.token != token {
            return false;
        }
        match result {
            Ok(()) => {
                // Transport confirmed; keep the write pending until the
                // readback settles so a failed readback marks it uncertain
                // rather than silently re-arming the old marker.
                write.confirmed = true;
            }
            Err(error) => {
                write.uncertain = true;
                state.error = Some(error);
            }
        }
        true
    }
}

/// Confirmed viewed/total counts over the listed files plus whether every
/// listed file is confirmed Viewed. Unknown and unviewed never count.
pub fn pr_viewed_progress(
    snapshot: Option<&PrViewedSnapshot>,
    listed_files: &[GitHubPrFile],
) -> (usize, usize, bool) {
    let total = listed_files.len();
    let Some(snapshot) = snapshot else {
        return (0, total, false);
    };
    let viewed = listed_files
        .iter()
        .filter(|file| snapshot.status(&file.path) == PrFileViewedStatus::Viewed)
        .count();
    (viewed, total, viewed == total && total > 0)
}

/// First file after `current` (wrapping once) whose marker is not confirmed
/// Viewed. Never returns `current`, so "no other target" disables the button.
/// Without a confirmed snapshot nothing is known-unviewed.
pub fn next_unviewed_file<'a>(
    snapshot: Option<&PrViewedSnapshot>,
    listed_files: &'a [GitHubPrFile],
    current: Option<&str>,
) -> Option<&'a str> {
    let snapshot = snapshot?;
    if listed_files.is_empty() {
        return None;
    }
    let current_index = current
        .and_then(|path| listed_files.iter().position(|file| file.path == path));
    let (start, limit) = match current_index {
        Some(index) => (index + 1, listed_files.len() - 1),
        None => (0, listed_files.len()),
    };
    listed_files
        .iter()
        .cycle()
        .skip(start)
        .take(limit)
        .map(|file| file.path.as_str())
        .find(|path| snapshot.status(path) != PrFileViewedStatus::Viewed)
}

/// Transport seam for viewed-marker reads/writes. Production calls the `gh`
/// transport in threadlane-git; interaction tests substitute delayed fakes so
/// they never shell out or mutate real pull requests.
#[derive(Clone)]
pub struct PrViewedTransport {
    /// (work_dir, pr_url) -> confirmed markers for the signed-in viewer.
    pub read:
        Arc<dyn Fn(PathBuf, String) -> Result<GitHubPrViewedState, String> + Send + Sync>,
    /// (work_dir, pr_url, pull_request_id, path, viewed) -> GitHub confirmed.
    pub write:
        Arc<dyn Fn(PathBuf, String, String, String, bool) -> Result<(), String> + Send + Sync>,
}

impl Default for PrViewedTransport {
    fn default() -> Self {
        Self {
            read: Arc::new(|work_dir, url| {
                threadlane_git::pull_request_viewed_state(&work_dir, &url)
                    .map_err(|error| error.message)
            }),
            write: Arc::new(|work_dir, url, pull_request_id, path, viewed| {
                threadlane_git::set_pull_request_file_viewed(
                    &work_dir,
                    &url,
                    &pull_request_id,
                    &path,
                    viewed,
                )
                .map_err(|error| error.message)
            }),
        }
    }
}

pub const INVALID_PR_REPLY_TARGET: &str =
    "This review comment can’t be replied to because GitHub returned an invalid comment ID.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrReplyTarget {
    pub remote_id: String,
    pub reply_to_remote_id: Option<String>,
    pub author: String,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<u64>,
}

impl PrReplyTarget {
    pub fn comment_id(&self) -> Result<u64, &'static str> {
        self.reply_to_remote_id
            .as_ref()
            .unwrap_or(&self.remote_id)
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or(INVALID_PR_REPLY_TARGET)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrCommentTarget {
    PullRequest,
    Reply(PrReplyTarget, u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrCommentAttempt {
    pub token: u64,
    pub key: PrWorkspaceKey,
    pub target: PrCommentTarget,
    pub body: String,
    pub pr_url: String,
    pub pre_write_ids: HashSet<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrCommentPhase {
    #[default]
    Idle,
    Publishing,
    Checking,
    Present,
    Absent,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrCommentControl {
    Post,
    ClearDraft,
    Retry,
    CheckAgain,
    PostNewDraft,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrCommentPublish {
    pub phase: PrCommentPhase,
    pub attempt: Option<PrCommentAttempt>,
    pub error: Option<String>,
}

impl PrCommentPublish {
    pub fn is_active(&self) -> bool {
        matches!(
            self.phase,
            PrCommentPhase::Publishing | PrCommentPhase::Checking
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrCommentDraft {
    pub body: String,
    pub publish: PrCommentPublish,
    pub reply: Option<PrReplyDraft>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrReplyDraft {
    pub target: PrReplyTarget,
    pub body: String,
    pub publish: PrCommentPublish,
    pub blocked: bool,
}

pub fn pr_publish_control(body: &str, publish: &PrCommentPublish) -> PrCommentControl {
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
pub struct PrCommentDrafts {
    pub by_pr: HashMap<PrWorkspaceKey, PrCommentDraft>,
    pub next_attempt_token: u64,
}

impl PrCommentDrafts {
    pub fn get(&self, key: &PrWorkspaceKey) -> Option<&PrCommentDraft> {
        self.by_pr.get(key)
    }

    pub fn set_body(&mut self, key: PrWorkspaceKey, body: String) {
        self.by_pr.entry(key).or_default().body = body;
    }

    pub fn select_reply_target(&mut self, key: PrWorkspaceKey, target: PrReplyTarget) -> bool {
        let draft = self.by_pr.entry(key).or_default();
        if let Some(reply) = draft.reply.as_mut() {
            if reply.target == target {
                reply.blocked = false;
                return true;
            }
            if !reply.body.is_empty()
                || reply.publish.phase != PrCommentPhase::Idle
                || reply.publish.attempt.is_some()
            {
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

    pub fn set_reply_body(&mut self, key: &PrWorkspaceKey, body: String) {
        if let Some(reply) = self
            .by_pr
            .get_mut(key)
            .and_then(|draft| draft.reply.as_mut())
        {
            reply.body = body;
            reply.blocked = false;
        }
    }

    pub fn begin(
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

    pub fn begin_reply(
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

    pub fn matching_publish_mut(
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

    pub fn mark_checking(&mut self, attempt: &PrCommentAttempt, error: String) -> bool {
        let Some(publish) = self.matching_publish_mut(attempt) else {
            return false;
        };
        publish.phase = PrCommentPhase::Checking;
        publish.error = Some(error);
        true
    }

    pub fn complete_success(&mut self, attempt: &PrCommentAttempt) -> bool {
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

    pub fn begin_recheck(&mut self, key: &PrWorkspaceKey) -> Option<PrCommentAttempt> {
        self.begin_recheck_for(key, false)
    }

    pub fn begin_reply_recheck(&mut self, key: &PrWorkspaceKey) -> Option<PrCommentAttempt> {
        self.begin_recheck_for(key, true)
    }

    pub fn begin_recheck_for(
        &mut self,
        key: &PrWorkspaceKey,
        reply: bool,
    ) -> Option<PrCommentAttempt> {
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

    pub fn complete_readback(
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

    pub fn clear(&mut self, key: &PrWorkspaceKey) {
        let draft = self.by_pr.entry(key.clone()).or_default();
        draft.body.clear();
        draft.publish = PrCommentPublish::default();
    }

    pub fn clear_reply(&mut self, key: &PrWorkspaceKey) {
        self.by_pr.entry(key.clone()).or_default().reply = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrReadback {
    Present,
    Absent,
    Unknown,
}

pub fn normalized_pr_body(body: &str) -> String {
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn classify_pr_readback(
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
        PrCommentTarget::Reply(..) => {
            if !detail.review_comments_complete {
                return PrReadback::Unknown;
            }
            detail.review_comments.iter().any(|comment| {
                !attempt.pre_write_ids.contains(&comment.remote_id)
                    && normalized_pr_body(&comment.body) == expected
            })
        }
    };
    if present {
        PrReadback::Present
    } else {
        PrReadback::Absent
    }
}

pub fn pr_publish_refresh_matches_selection(
    attempt: &PrCommentAttempt,
    selected: Option<&PrWorkspaceKey>,
) -> bool {
    selected == Some(&attempt.key)
}

pub fn pr_present_action_id(reply: bool, attempt: &PrCommentAttempt) -> SharedString {
    let target = match &attempt.target {
        PrCommentTarget::PullRequest => "pull-request",
        PrCommentTarget::Reply(target, _) => target.remote_id.as_str(),
    };
    format!(
        "github-pr-{}-open-present-{}-{}-{target}",
        if reply { "reply" } else { "comment" },
        attempt.key.project.display(),
        attempt.key.number
    )
    .into()
}

pub fn pr_present_recovery_action(reply: bool, attempt: &PrCommentAttempt, cx: &gpui::App) -> Link {
    threadlane_ui_kit::github::github_draft_recovery(
        pr_present_action_id(reply, attempt), attempt.pr_url.clone(), cx,
    )
}

pub enum GitHubListResult {
    Issues(ScopedIssueList),
    PullRequests(ScopedPrList),
}

#[derive(Clone, Debug)]
pub struct ScopedIssueList {
    pub rows: Vec<ScopedIssue>,
    pub errors: Vec<String>,
    pub has_more: bool,
}

#[derive(Clone, Debug)]
pub struct ScopedPrList {
    pub rows: Vec<ScopedPr>,
    pub errors: Vec<String>,
    pub has_more: bool,
}

pub fn scoped_list_error(errors: &[String]) -> Option<String> {
    errors.first().cloned()
}

pub enum GitHubDetailResult {
    Issue(Result<GitHubIssueDetail, String>),
    PullRequest(Result<GitHubPrInfo, String>),
}

#[derive(Clone, Debug)]
pub struct LinkedSession {
    pub project_name: String,
    pub session: SessionInfo,
    pub status: &'static str,
    pub branch: Option<String>,
    pub pr_number: Option<u64>,
}
