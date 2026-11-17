//! In-memory fakes for the interfaces of Plan.md §3.4 that are not stores or
//! backends. Stores are faked by `HubDb::open_in_memory` in `threadlane-store`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::adapters::{
    ChannelAdapter, CheckpointBundle, Delivered, DeliveryError, PreparedWorkspace, SourceAdapter,
    SourceBatch, SourceError, SourceEvent, Workspace, WorkspaceError, WorkspaceSpec,
};
use threadlane_core::model::OutboxMessage;
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::tools::{
    GateRequest, PermissionPolicy, ToolContext, ToolDefinition, ToolEndpoint, ToolError,
    ToolRegistry, Verdict,
};

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

/// A [`PermissionPolicy`] that returns a verdict per tool, or a default.
#[derive(Debug, Clone)]
pub struct FixedPolicy {
    default: Verdict,
    per_tool: Arc<Mutex<HashMap<String, Verdict>>>,
    requests: Arc<Mutex<Vec<GateRequest>>>,
}

impl FixedPolicy {
    /// A policy that answers `default` for every tool not set otherwise.
    #[must_use]
    pub fn new(default: Verdict) -> Self {
        Self {
            default,
            per_tool: Arc::default(),
            requests: Arc::default(),
        }
    }

    /// Uses `verdict` for every call to `tool`.
    pub fn set(&self, tool: impl Into<String>, verdict: Verdict) {
        lock(&self.per_tool).insert(tool.into(), verdict);
    }

    /// Every request the gate saw.
    #[must_use]
    pub fn requests(&self) -> Vec<GateRequest> {
        lock(&self.requests).clone()
    }
}

#[async_trait]
impl PermissionPolicy for FixedPolicy {
    async fn decide(&self, request: &GateRequest) -> Verdict {
        lock(&self.requests).push(request.clone());
        lock(&self.per_tool)
            .get(&request.tool)
            .cloned()
            .unwrap_or_else(|| self.default.clone())
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
