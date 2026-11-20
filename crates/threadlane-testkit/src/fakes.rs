//! In-memory fakes for the interfaces of Plan.md §3.4 that are not stores or
//! backends. Stores are faked by `HubDb::open_in_memory` in `threadlane-store`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::adapters::{
    ChannelAdapter, CheckpointBundle, Delivered, DeliveryError, PreparedWorkspace, SourceAdapter,
    SourceBatch, SourceError, SourceEvent, Workspace, WorkspaceError, WorkspaceSpec,
};
use threadlane_core::forge::{ApprovedBranch, Forge, ForgeError, PullRequestUpdate};
use threadlane_core::github::{
    Feed, GithubError, GithubItem, GithubSource, IssueClaim, PullTarget,
};
use threadlane_core::model::{OutboxMessage, Repository};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::tools::{ToolContext, ToolDefinition, ToolEndpoint, ToolError, ToolRegistry};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().expect("fake lock poisoned")
}

/// A [`Waker`] that records every hint.
#[derive(Debug, Clone, Default)]
pub struct RecordingWaker {
    hints: Arc<Mutex<Vec<WakeTarget>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl RecordingWaker {
    /// A waker with no hints recorded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every hint sent so far.
    #[must_use]
    pub fn hints(&self) -> Vec<WakeTarget> {
        lock(&self.hints).clone()
    }

    /// Completes after the next hint.
    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

impl Waker for RecordingWaker {
    fn wake(&self, target: WakeTarget) {
        lock(&self.hints).push(target);
        self.notify.notify_waiters();
    }
}

/// A [`ChannelAdapter`] that records deliveries and can be told to fail.
#[derive(Debug, Clone)]
pub struct RecordingChannel {
    name: String,
    delivered: Arc<Mutex<Vec<OutboxMessage>>>,
    failures: Arc<Mutex<VecDeque<DeliveryError>>>,
}

impl RecordingChannel {
    /// A channel named `name` that delivers everything.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            delivered: Arc::default(),
            failures: Arc::default(),
        }
    }

    /// Makes the next delivery fail with `error`.
    pub fn fail_next(&self, error: DeliveryError) {
        lock(&self.failures).push_back(error);
    }

    /// Every message delivered so far.
    #[must_use]
    pub fn delivered(&self) -> Vec<OutboxMessage> {
        lock(&self.delivered).clone()
    }
}

#[async_trait]
impl ChannelAdapter for RecordingChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn deliver(&self, message: &OutboxMessage) -> Result<Delivered, DeliveryError> {
        if let Some(error) = lock(&self.failures).pop_front() {
            return Err(error);
        }
        let mut delivered = lock(&self.delivered);
        delivered.push(message.clone());
        Ok(Delivered {
            channel_message_id: Some(format!("{}-{}", self.name, delivered.len())),
        })
    }
}

/// One call recorded by [`FakeTools`].
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: Json,
}

/// Tools that return canned results and record their calls. Implements both
/// [`ToolEndpoint`] (what a backend calls) and [`ToolRegistry`].
#[derive(Debug, Clone, Default)]
pub struct FakeTools {
    definitions: Arc<Mutex<Vec<ToolDefinition>>>,
    results: Arc<Mutex<HashMap<String, Result<Json, ToolError>>>>,
    calls: Arc<Mutex<Vec<ToolCallRecord>>>,
}

impl FakeTools {
    /// No tools registered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a tool and the result every call to it returns.
    pub fn register(&self, definition: ToolDefinition, result: Result<Json, ToolError>) {
        lock(&self.results).insert(definition.name.clone(), result);
        lock(&self.definitions).push(definition);
    }

    /// Every call made so far.
    #[must_use]
    pub fn calls(&self) -> Vec<ToolCallRecord> {
        lock(&self.calls).clone()
    }

    fn respond(
        &self,
        call_id: Option<&str>,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        lock(&self.calls).push(ToolCallRecord {
            call_id: call_id.map(str::to_owned),
            name: name.to_owned(),
            arguments,
        });
        lock(&self.results)
            .get(name)
            .cloned()
            .unwrap_or_else(|| Err(ToolError::UnknownTool(name.to_owned())))
    }
}

#[async_trait]
impl ToolEndpoint for FakeTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        lock(&self.definitions).clone()
    }

    async fn call(
        &self,
        call_id: Option<&str>,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        self.respond(call_id, name, arguments)
    }
}

#[async_trait]
impl ToolRegistry for FakeTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        lock(&self.definitions).clone()
    }

    fn definition(&self, name: &str) -> Option<ToolDefinition> {
        lock(&self.definitions)
            .iter()
            .find(|definition| definition.name == name)
            .cloned()
    }

    async fn execute(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        self.respond(context.call_id.as_deref(), name, arguments)
    }
}

/// A [`SourceAdapter`] that hands out queued batches.
#[derive(Debug, Clone)]
pub struct QueuedSource {
    source: String,
    batches: Arc<Mutex<VecDeque<Result<SourceBatch, SourceError>>>>,
    cursors: Arc<Mutex<Vec<Option<String>>>>,
}

impl QueuedSource {
    /// A source named `source` with nothing queued; pulls return empty batches.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            batches: Arc::default(),
            cursors: Arc::default(),
        }
    }

    /// Queues the result of the next pull.
    pub fn push(&self, events: Vec<SourceEvent>, cursor: Option<String>) {
        lock(&self.batches).push_back(Ok(SourceBatch { events, cursor }));
    }

    /// Makes the next pull fail.
    pub fn push_error(&self, error: SourceError) {
        lock(&self.batches).push_back(Err(error));
    }

    /// The cursor each pull was given.
    #[must_use]
    pub fn cursors(&self) -> Vec<Option<String>> {
        lock(&self.cursors).clone()
    }
}

#[async_trait]
impl SourceAdapter for QueuedSource {
    fn source(&self) -> &str {
        &self.source
    }

    async fn pull(&self, cursor: Option<&str>) -> Result<SourceBatch, SourceError> {
        lock(&self.cursors).push(cursor.map(str::to_owned));
        lock(&self.batches)
            .pop_front()
            .unwrap_or_else(|| Ok(SourceBatch::default()))
    }
}

/// A [`Workspace`] that touches no disk: it hands out paths under `root` and
/// records checkpoints.
#[derive(Debug, Clone)]
pub struct FakeWorkspace {
    root: PathBuf,
    checkpoints: Arc<Mutex<Vec<PreparedWorkspace>>>,
}

impl FakeWorkspace {
    /// A workspace handing out paths under `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            checkpoints: Arc::default(),
        }
    }

    /// How many checkpoints were taken.
    #[must_use]
    pub fn checkpoint_count(&self) -> usize {
        lock(&self.checkpoints).len()
    }

    fn layout(&self, spec: &WorkspaceSpec) -> PreparedWorkspace {
        let root = self.root.join(spec.task_id.to_string());
        PreparedWorkspace {
            worktrees: spec
                .repositories
                .iter()
                .map(|repository| (repository.repository_id, root.join(&repository.name)))
                .collect(),
            root,
        }
    }
}

#[async_trait]
impl Workspace for FakeWorkspace {
    async fn prepare(&self, spec: &WorkspaceSpec) -> Result<PreparedWorkspace, WorkspaceError> {
        Ok(self.layout(spec))
    }

    async fn checkpoint(
        &self,
        workspace: &PreparedWorkspace,
    ) -> Result<Vec<CheckpointBundle>, WorkspaceError> {
        let mut checkpoints = lock(&self.checkpoints);
        checkpoints.push(workspace.clone());
        let n = checkpoints.len();
        Ok(workspace
            .worktrees
            .iter()
            .map(|(repository_id, path)| CheckpointBundle {
                repository_id: *repository_id,
                base_commit: "base".into(),
                branch: format!("threadlane/{n}"),
                bundle_path: path.with_extension("bundle"),
                sha256: format!("fake-{n}"),
            })
            .collect())
    }

    async fn restore(
        &self,
        spec: &WorkspaceSpec,
        _bundles: &[CheckpointBundle],
    ) -> Result<PreparedWorkspace, WorkspaceError> {
        Ok(self.layout(spec))
    }
}

/// One call a [`FakeForge`] received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeCall {
    /// An approved issue, pull request comment or merge.
    Apply {
        remote: String,
        action: threadlane_core::forge::RepositoryAction,
    },
    Push {
        worktree: PathBuf,
        branch: String,
    },
    FastForward {
        repository: String,
        number: u64,
        expected_head: String,
        revision: String,
    },
    Open {
        worktree: PathBuf,
        branch: String,
        base: String,
        title: String,
        body: String,
        opened_by: String,
    },
    Find {
        branch: String,
    },
}

/// A [`Forge`] that touches no remote: it records every call, and each
/// [`Forge::open_draft_pull_request`] opens a new pull request, so a test
/// sees a second one if the system ever opens two. Clones share their record.
#[derive(Debug, Clone, Default)]
pub struct FakeForge {
    calls: Arc<Mutex<Vec<ForgeCall>>>,
    /// The pull requests opened, as (head branch, URL), oldest first.
    opened: Arc<Mutex<Vec<(String, String)>>>,
    /// Reported by [`Forge::logged_in`] inverted, so the default is logged in.
    logged_out: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeForge {
    /// Sets what [`Forge::logged_in`] reports; logged in until set.
    pub fn set_logged_in(&self, logged_in: bool) {
        self.logged_out
            .store(!logged_in, std::sync::atomic::Ordering::Relaxed);
    }

    /// Every call, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<ForgeCall> {
        lock(&self.calls).clone()
    }

    /// The URLs of the pull requests opened, oldest first.
    #[must_use]
    pub fn pull_requests(&self) -> Vec<String> {
        lock(&self.opened)
            .iter()
            .map(|(_, url)| url.clone())
            .collect()
    }

    /// The branches pushed, once per push.
    #[must_use]
    pub fn pushes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                ForgeCall::Push { branch, .. } => Some(branch),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl Forge for FakeForge {
    async fn apply(
        &self,
        remote: &str,
        action: &threadlane_core::forge::RepositoryAction,
    ) -> Result<String, ForgeError> {
        lock(&self.calls).push(ForgeCall::Apply {
            remote: remote.to_owned(),
            action: action.clone(),
        });
        Ok("done".into())
    }

    async fn push(&self, worktree: &Path, branch: ApprovedBranch<'_>) -> Result<(), ForgeError> {
        lock(&self.calls).push(ForgeCall::Push {
            worktree: worktree.to_owned(),
            branch: branch.name.to_owned(),
        });
        Ok(())
    }

    async fn fast_forward_pull_request(
        &self,
        _worktree: &Path,
        repository: &Repository,
        update: PullRequestUpdate<'_>,
    ) -> Result<String, ForgeError> {
        lock(&self.calls).push(ForgeCall::FastForward {
            repository: repository.name.clone(),
            number: update.number.get(),
            expected_head: update.expected_head.to_owned(),
            revision: update.revision.to_owned(),
        });
        Ok("feature/pr-head".into())
    }

    async fn open_draft_pull_request(
        &self,
        worktree: &Path,
        branch: ApprovedBranch<'_>,
        base: &str,
        title: &str,
        body: &str,
        opened_by: &str,
    ) -> Result<String, ForgeError> {
        self.push(worktree, branch).await?;
        lock(&self.calls).push(ForgeCall::Open {
            worktree: worktree.to_owned(),
            branch: branch.name.to_owned(),
            base: base.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            opened_by: opened_by.to_owned(),
        });
        let mut opened = lock(&self.opened);
        let url = format!("https://forge.test/pull/{}", opened.len() + 1);
        opened.push((branch.name.to_owned(), url.clone()));
        Ok(url)
    }

    async fn logged_in(&self) -> bool {
        !self.logged_out.load(std::sync::atomic::Ordering::Relaxed)
    }

    async fn find_pull_request(
        &self,
        _worktree: &Path,
        branch: &str,
    ) -> Result<Option<String>, ForgeError> {
        lock(&self.calls).push(ForgeCall::Find {
            branch: branch.to_owned(),
        });
        Ok(lock(&self.opened)
            .iter()
            .find(|(head, _)| head == branch)
            .map(|(_, url)| url.clone()))
    }
}

/// One call a [`FakeGithub`] was asked to make. Each is a read: the
/// interface has no other kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubCall {
    Notifications {
        cursor: Option<String>,
    },
    PullRequest {
        target: PullTarget,
        since: Option<String>,
    },
    IssueClaim {
        repository: String,
        number: u64,
    },
}

#[derive(Debug, Default)]
struct GithubState {
    calls: Vec<GithubCall>,
    notifications: Vec<GithubItem>,
    /// Bumped by every notification, so the feed knows what changed.
    version: u64,
    /// Items on a task's work, by `(repository, branch)`.
    work: Vec<(String, String, GithubItem)>,
    poll_interval: Option<std::time::Duration>,
    /// Notification id, details available, and whether the lookup succeeded.
    activity: Vec<(String, Vec<GithubItem>, bool)>,
    /// What [`GithubSource::issue_claim`] answers, by `(repository, number)`.
    claims: Vec<(String, u64, Result<IssueClaim, String>)>,
}

/// A [`GithubSource`] that touches no network. It records every call, keeps
/// every notification unread, as GitHub does when nothing marks it read,
/// and answers "not modified" when nothing was added since the cursor.
/// Clones share their state.
#[derive(Debug, Clone, Default)]
pub struct FakeGithub {
    state: Arc<Mutex<GithubState>>,
}

impl FakeGithub {
    /// Adds a notification to the feed.
    pub fn notify(&self, item: GithubItem) {
        let mut state = lock(&self.state);
        state.notifications.push(item);
        state.version += 1;
    }

    /// Adds an item on the work of `branch` in `repository`.
    pub fn add_to(&self, repository: &str, branch: &str, item: GithubItem) {
        lock(&self.state)
            .work
            .push((repository.to_owned(), branch.to_owned(), item));
    }

    /// What the notification `id` reports, as [`GithubSource::activity`].
    /// The latest call for an `id` wins.
    pub fn set_activity(&self, id: &str, items: Vec<GithubItem>) {
        lock(&self.state)
            .activity
            .push((id.to_owned(), items, true));
    }

    /// Makes [`GithubSource::activity`] fail for the notification `id`.
    pub fn fail_activity(&self, id: &str, partial: Vec<GithubItem>) {
        lock(&self.state)
            .activity
            .push((id.to_owned(), partial, false));
    }

    /// What [`GithubSource::issue_claim`] answers for issue `number` of
    /// `repository`: the claim, or the error. Unscripted issues fail.
    pub fn set_claim(&self, repository: &str, number: u64, claim: Result<IssueClaim, String>) {
        lock(&self.state)
            .claims
            .push((repository.to_owned(), number, claim));
    }

    /// What the feed answers as the interval it asks for.
    pub fn set_poll_interval(&self, interval: Option<std::time::Duration>) {
        lock(&self.state).poll_interval = interval;
    }

    /// Every call, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<GithubCall> {
        lock(&self.state).calls.clone()
    }
}

#[async_trait]
impl GithubSource for FakeGithub {
    async fn notifications(&self, cursor: Option<&str>) -> Result<Feed, GithubError> {
        let mut state = lock(&self.state);
        state.calls.push(GithubCall::Notifications {
            cursor: cursor.map(str::to_owned),
        });
        let version = state.version.to_string();
        if cursor == Some(version.as_str()) {
            return Ok(Feed {
                not_modified: true,
                poll_interval: state.poll_interval,
                ..Feed::default()
            });
        }
        Ok(Feed {
            not_modified: false,
            items: state.notifications.clone(),
            cursor: Some(version),
            poll_interval: state.poll_interval,
        })
    }

    async fn pull_request(
        &self,
        target: &PullTarget,
        since: Option<&str>,
    ) -> Result<Vec<GithubItem>, GithubError> {
        let mut state = lock(&self.state);
        state.calls.push(GithubCall::PullRequest {
            target: target.clone(),
            since: since.map(str::to_owned),
        });
        let mut items: Vec<GithubItem> = state
            .work
            .iter()
            .filter(|(repository, branch, item)| {
                *repository == target.repository
                    // A target known by number only is read on its pull request's items.
                    && (*branch == target.branch
                        || (target.branch.is_empty() && item.pull_request == target.number))
                    && since.is_none_or(|since| item.at.as_str() >= since)
            })
            .map(|(_, _, item)| item.clone())
            .collect();
        items.sort_by(|a, b| a.at.cmp(&b.at));
        Ok(items)
    }

    async fn activity(
        &self,
        notification: &GithubItem,
        items: &mut Vec<GithubItem>,
    ) -> Result<(), GithubError> {
        let state = lock(&self.state);
        if let Some((_, found, complete)) = state
            .activity
            .iter()
            .rev()
            .find(|(id, _, _)| *id == notification.id)
        {
            items.extend_from_slice(found);
            if !complete {
                return Err(GithubError(format!("{} failed", notification.id)));
            }
        } else {
            items.push(notification.clone());
        }
        Ok(())
    }

    async fn issue_claim(&self, repository: &str, number: u64) -> Result<IssueClaim, GithubError> {
        let mut state = lock(&self.state);
        state.calls.push(GithubCall::IssueClaim {
            repository: repository.to_owned(),
            number,
        });
        state
            .claims
            .iter()
            .rev()
            .find(|(r, n, _)| r == repository && *n == number)
            .map_or_else(
                || Err(GithubError(format!("no claim for {repository}#{number}"))),
                |(_, _, claim)| claim.clone().map_err(GithubError),
            )
    }
}
