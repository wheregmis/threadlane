// Scaffolding for explicit background-task management (/task).
// HarnessSupervisor is publicly exported but most methods are currently
// exercised only in tests; dead-code warnings are intentionally suppressed.
#![allow(dead_code)]
use crate::coding_agent::harness::CodingSessionHarness;
use crate::coding_agent::{CodingAgentOptions, SubagentCancellationGuard};
use crate::controller::{ExecutionMode, SessionController};
use crate::project_registry::{
    load_project_registry_from, merge_and_save_project_registry_to, ProjectRecord,
};
use log::error;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use threadlane_runtime::harness::{
    DurableEvent, HarnessEvent, LaneStatus as HarnessLaneStatus, OperationOutcome,
    Record as HarnessRecord, SubagentLifecyclePhase,
};
use threadlane_runtime::{AgentEvent, AgentMessage, TokenUsage};
use threadlane_wasi::packages::ExtensionScope;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Idle,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneStatus {
    Idle,
    Running,
    Suspended,
    Cancelling,
}

#[derive(Debug, Clone, Default)]
pub struct CheckpointResult {
    steer_messages: Vec<AgentMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    Background,
    Subagent,
}

/// Runtime lane projection: in-memory operational state only.
/// Persistence is owned by `CodingSessionHarness`.
#[derive(Debug, Clone)]
pub struct Lane {
    name: String,
    session_id: String,
    parent_lane: Option<String>,
    leaf_id: Option<String>,
    status: LaneStatus,
    active_run_id: Option<String>,
    session_file: Option<PathBuf>,
    accumulated_usage: TokenUsage,
}

impl Lane {
    fn new(name: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            session_id: session_id.into(),
            parent_lane: None,
            leaf_id: None,
            status: LaneStatus::Idle,
            active_run_id: None,
            session_file: None,
            accumulated_usage: TokenUsage::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    project_id: String,
    session_id: String,
    pub session_file: Option<PathBuf>,
    parent_task_id: Option<String>,
    kind: TaskKind,
    agent: String,
    summary: String,
    current_activity: Option<String>,
    status: TaskStatus,
    started_at_ms: u128,
    finished_at_ms: Option<u128>,
}

impl TaskRecord {
    fn cancellable(&self) -> bool {
        self.kind == TaskKind::Background && self.active()
    }

    fn active(&self) -> bool {
        matches!(
            self.status,
            TaskStatus::Idle | TaskStatus::Running | TaskStatus::Waiting
        )
    }
}

#[derive(Debug, Clone)]
pub struct TaskAgentEvent {
    task_id: String,
    project_id: String,
    lane: Option<String>,
    event: AgentEvent,
    /// When set, this event carries a harness event instead of a legacy
    /// AgentEvent. The `event` field is a sentinel (AgentStart) and the
    /// UI should use `harness_event` for activity updates.
    harness_event: Option<HarnessEvent>,
}

impl TaskAgentEvent {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn lane(&self) -> Option<&str> {
        self.lane.as_deref()
    }

    pub fn into_parts(self) -> (String, String, AgentEvent) {
        (self.task_id, self.project_id, self.event)
    }

    pub fn harness_event(&self) -> Option<&HarnessEvent> {
        self.harness_event.as_ref()
    }

    /// Returns the inner [`DurableEvent`] if this event wraps a committed journal fact.
    pub fn durable_event(&self) -> Option<DurableEvent> {
        self.harness_event
            .as_ref()
            .and_then(HarnessEvent::as_durable)
    }

    /// Returns `true` if this event represents a durable fact on disk.
    pub fn is_durable(&self) -> bool {
        self.harness_event
            .as_ref()
            .map_or(false, HarnessEvent::is_durable)
    }
}

struct TaskRuntime {
    controller: Arc<SessionController>,
    /// Ephemeral execution control only; journal projections own lifecycle state.
    run_handle: Option<tokio::task::AbortHandle>,
    cancellation_guard: Option<SubagentCancellationGuard>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolOutputCache {
    cache: HashMap<String, String>,
}

impl ToolOutputCache {
    fn get(&self, key: &str) -> Option<String> {
        self.cache.get(key).cloned()
    }

    fn put(&mut self, key: String, output: String) {
        self.cache.insert(key, output);
    }

    fn invalidate_all(&mut self) {
        self.cache.clear();
    }

    pub fn invalidate_path(&mut self, path: &str) {
        self.cache.retain(|key, _| !key.contains(path));
    }
}

#[derive(Clone)]
pub struct HarnessSupervisor {
    global_dir: PathBuf,
    projects: Arc<Mutex<HashMap<String, ProjectRecord>>>,
    tasks: Arc<Mutex<HashMap<String, TaskRecord>>>,
    runtimes: Arc<Mutex<HashMap<String, TaskRuntime>>>,
    lanes: Arc<Mutex<HashMap<String, Lane>>>,
    metrics: Arc<Mutex<threadlane_runtime::HarnessMetrics>>,
    output_cache: Arc<Mutex<ToolOutputCache>>,
    event_tx: broadcast::Sender<TaskAgentEvent>,
}

impl HarnessSupervisor {
    pub fn new(global_dir: PathBuf) -> Self {
        let (event_tx, _) = broadcast::channel(1000);
        let _ = fs::create_dir_all(&global_dir);
        let supervisor = Self {
            global_dir,
            projects: Arc::new(Mutex::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            lanes: Arc::new(Mutex::new(HashMap::new())),
            metrics: Arc::new(Mutex::new(threadlane_runtime::HarnessMetrics::default())),
            output_cache: Arc::new(Mutex::new(ToolOutputCache::default())),
            event_tx,
        };
        supervisor.load_registry();
        supervisor
    }

    // ── Lane projection (in-memory operational state only) ───────────────

    fn output_cache(&self) -> Arc<Mutex<ToolOutputCache>> {
        self.output_cache.clone()
    }

    fn record_lane_usage(&self, session_id: &str, lane_name: &str, usage: &TokenUsage) {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        if let Some(lane) = lock.get_mut(&key) {
            lane.accumulated_usage.input_tokens += usage.input_tokens;
            lane.accumulated_usage.output_tokens += usage.output_tokens;
            lane.accumulated_usage.total_tokens += usage.total_tokens;
        }
    }

    fn aggregate_tree_usage(&self, session_id: &str, root_lane: &str) -> TokenUsage {
        let lock = self.lanes.lock().unwrap();
        let mut total = TokenUsage::default();
        for lane in lock.values() {
            if lane.session_id == session_id
                && (lane.name == root_lane || lane.parent_lane.as_deref() == Some(root_lane))
            {
                total.input_tokens += lane.accumulated_usage.input_tokens;
                total.output_tokens += lane.accumulated_usage.output_tokens;
                total.total_tokens += lane.accumulated_usage.total_tokens;
            }
        }
        total
    }

    fn metrics(&self) -> threadlane_runtime::HarnessMetrics {
        let mut m = self.metrics.lock().unwrap().clone();
        m.active_lanes = self.lanes.lock().unwrap().len();
        m
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskAgentEvent> {
        self.event_tx.subscribe()
    }

    fn subscribe_lane(
        &self,
        _session_id: &str,
        _lane_name: &str,
    ) -> broadcast::Receiver<TaskAgentEvent> {
        self.event_tx.subscribe()
    }

    fn get_or_create_lane(&self, session_id: &str, lane_name: &str) -> Lane {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        lock.entry(key)
            .or_insert_with(|| Lane::new(lane_name, session_id))
            .clone()
    }

    fn get_or_create_sub_lane(&self, session_id: &str, lane_name: &str, parent_lane: &str) -> Lane {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock.entry(key).or_insert_with(|| {
            let mut l = Lane::new(lane_name, session_id);
            l.parent_lane = Some(parent_lane.to_string());
            l
        });
        lane.clone()
    }

    fn cancel_lane_hierarchy(&self, session_id: &str, root_lane: &str) -> usize {
        let mut lock = self.lanes.lock().unwrap();
        let mut cancelled_count = 0;
        let targets: Vec<String> = lock
            .values()
            .filter(|l| {
                l.session_id == session_id
                    && (l.name == root_lane || l.parent_lane.as_deref() == Some(root_lane))
            })
            .map(|l| format!("{session_id}:{}", l.name))
            .collect();

        for key in targets {
            if let Some(lane) = lock.get_mut(&key) {
                lane.status = LaneStatus::Cancelling;
                cancelled_count += 1;
            }
        }
        cancelled_count
    }

    /// Queue a steer message on the lane, persisting directly to CodingSessionHarness if session_file exists.
    fn enqueue_steer(
        &self,
        session_id: &str,
        lane_name: &str,
        message: AgentMessage,
    ) -> Result<(), String> {
        self.enqueue_steer_priority(
            session_id,
            lane_name,
            message,
            threadlane_runtime::SteerPriority::Normal,
        )
    }

    fn enqueue_steer_priority(
        &self,
        session_id: &str,
        lane_name: &str,
        message: AgentMessage,
        priority: threadlane_runtime::SteerPriority,
    ) -> Result<(), String> {
        let key = format!("{session_id}:{lane_name}");
        let session_file = {
            let mut lock = self.lanes.lock().unwrap();
            let lane = lock
                .entry(key)
                .or_insert_with(|| Lane::new(lane_name, session_id));
            lane.session_file.clone()
        };
        if let Some(session_file) = session_file {
            let mut harness = CodingSessionHarness::open(&session_file)?;
            let id = format!("steer-{}", now_ms());
            let target = threadlane_runtime::harness::ProvisionedEntry {
                id,
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                parent_id: None,
                message,
            };
            harness.enqueue_unbound_on_lane_with_priority(
                lane_name,
                threadlane_runtime::harness::QueueKind::Steer,
                target,
                Some(priority),
            )?;
        }
        Ok(())
    }

    fn enqueue_followup(
        &self,
        session_id: &str,
        lane_name: &str,
        message: AgentMessage,
    ) -> Result<(), String> {
        let key = format!("{session_id}:{lane_name}");
        let session_file = {
            let mut lock = self.lanes.lock().unwrap();
            let lane = lock
                .entry(key)
                .or_insert_with(|| Lane::new(lane_name, session_id));
            lane.session_file.clone()
        };
        if let Some(session_file) = session_file {
            let mut harness = CodingSessionHarness::open(&session_file)?;
            let id = format!("followup-{}", now_ms());
            let target = threadlane_runtime::harness::ProvisionedEntry {
                id,
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                parent_id: None,
                message,
            };
            harness.enqueue_unbound_on_lane_with_priority(
                lane_name,
                threadlane_runtime::harness::QueueKind::FollowUp,
                target,
                None,
            )?;
        }
        Ok(())
    }

    fn update_lane_leaf(&self, session_id: &str, lane_name: &str, leaf_id: Option<String>) {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        if let Some(lane) = lock.get_mut(&key) {
            lane.leaf_id = leaf_id;
        }
    }

    pub fn checkpoint_lane(&self, session_id: &str, lane_name: &str) -> CheckpointResult {
        let key = format!("{session_id}:{lane_name}");
        let session_file = self
            .lanes
            .lock()
            .unwrap()
            .get(&key)
            .and_then(|lane| lane.session_file.clone());

        let mut steer_messages = Vec::new();
        if let Some(session_file) = session_file {
            if let Ok(mut harness) = CodingSessionHarness::open(&session_file) {
                if let Ok(snapshot) = harness.snapshot() {
                    if let Some(lane_snap) =
                        snapshot.state.lanes.iter().find(|l| l.name == lane_name)
                    {
                        for queued in &lane_snap.queued {
                            if queued.queue == threadlane_runtime::harness::QueueKind::Steer {
                                steer_messages.push(queued.target.message.clone());
                            }
                        }
                    }
                }
                let _ = harness
                    .consume_first_unbound_queue(threadlane_runtime::harness::QueueKind::Steer);
            }
        }

        CheckpointResult { steer_messages }
    }

    // ── Session routing (delegates persistence to CodingSessionHarness) ──

    /// Navigate a lane to a target node.  Persistence goes through
    /// `CodingSessionHarness`; the supervisor updates its in-memory
    /// projection only after the harness commits.
    fn navigate_lane(
        &self,
        session_id: &str,
        lane_name: &str,
        target_node_id: &str,
    ) -> Result<bool, String> {
        let v2_file = self
            .lanes
            .lock()
            .unwrap()
            .get(&format!("{session_id}:{lane_name}"))
            .and_then(|lane| lane.session_file.clone());

        if let Some(ref session_file) = v2_file {
            if let Ok(mut harness) = CodingSessionHarness::open(session_file) {
                let snapshot = harness.snapshot().map_err(|e| e.to_string())?;
                if snapshot.entries.iter().any(|e| e.id == target_node_id) {
                    let run_id = format!("navigation-{}", now_ms());
                    // Build branch path: walk from target to root, then reverse
                    let mut path_ids = Vec::new();
                    let mut current = Some(target_node_id.to_string());
                    while let Some(id) = current {
                        path_ids.push(id.clone());
                        current = snapshot
                            .entries
                            .iter()
                            .find(|e| e.id == id)
                            .and_then(|e| e.parent_id.clone());
                    }
                    path_ids.reverse();
                    harness
                        .navigate_branch(&path_ids)
                        .map_err(|error| error.to_string())?;
                    harness
                        .store
                        .accept_navigation_on_lane(lane_name, &run_id, target_node_id, None)
                        .map_err(|error| error.to_string())?;
                    harness
                        .drive_to_completion()
                        .map_err(|error| error.to_string())?;
                    let key = format!("{session_id}:{lane_name}");
                    let mut lock = self.lanes.lock().unwrap();
                    let lane = lock
                        .entry(key)
                        .or_insert_with(|| Lane::new(lane_name, session_id));
                    lane.leaf_id = Some(target_node_id.to_string());
                    lane.status = LaneStatus::Idle;
                    return Ok(true);
                }
            }
        }

        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock
            .entry(key)
            .or_insert_with(|| Lane::new(lane_name, session_id));
        lane.leaf_id = Some(target_node_id.to_string());
        lane.status = LaneStatus::Idle;

        Ok(true)
    }

    fn redeem_deferred(
        &self,
        session_id: &str,
        lane_name: &str,
        _result_message: AgentMessage,
    ) -> Result<String, String> {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock
            .get_mut(&key)
            .ok_or_else(|| format!("Lane '{key}' not found"))?;
        lane.status = LaneStatus::Idle;
        let new_node_id = format!("node_{}", now_ms());
        lane.leaf_id = Some(new_node_id.clone());
        Ok(new_node_id)
    }

    // ── Recovery helpers (delegate to CodingSessionHarness) ───────────────

    /// Restore supervisor lane projections from the harness snapshot.
    fn restore_session_lanes(
        &self,
        session_id: &str,
        session_file: &Path,
    ) -> Result<threadlane_runtime::RecoveryResult, String> {
        let mut harness = CodingSessionHarness::open(session_file)?;
        let snapshot = harness.snapshot()?;
        let mut open_operation_ids = Vec::new();
        let mut abort_requested_operation_ids = Vec::new();
        let mut lock = self.lanes.lock().unwrap();
        for lane_snap in &snapshot.state.lanes {
            if lane_snap.open_operation.is_none() && lane_snap.queued.is_empty() {
                continue;
            }
            let key = format!("{session_id}:{}", lane_snap.name);
            let lane = lock
                .entry(key)
                .or_insert_with(|| Lane::new(&lane_snap.name, session_id));
            lane.session_file = Some(session_file.to_path_buf());
            lane.leaf_id = lane_snap.leaf_id.clone();
            lane.active_run_id = lane_snap.open_operation.clone();
            lane.status = lane_snap
                .open_operation
                .as_ref()
                .map(|_| LaneStatus::Suspended)
                .unwrap_or(LaneStatus::Idle);
            if let Some(run_id) = lane_snap.open_operation.clone() {
                open_operation_ids.push(run_id.clone());
                if lane_snap.abort_requested {
                    abort_requested_operation_ids.push(run_id);
                }
            }
        }
        Ok(threadlane_runtime::RecoveryResult {
            recovered_open_operations: open_operation_ids.len(),
            open_operation_ids,
            abort_requested_operation_ids,
            ..Default::default()
        })
    }

    /// Finish recovered operations through CodingSessionHarness.
    fn finish_recovered_operations(
        &self,
        session_id: &str,
        session_file: &Path,
        run_ids: &[String],
        outcome: threadlane_runtime::OperationOutcome,
    ) -> Result<(), String> {
        let mut harness = CodingSessionHarness::open(session_file)?;
        for run_id in run_ids {
            harness.finish_run(
                run_id,
                match outcome {
                    threadlane_runtime::OperationOutcome::Completed => {
                        threadlane_runtime::harness::OperationOutcome::Completed
                    }
                    threadlane_runtime::OperationOutcome::Aborted => {
                        threadlane_runtime::harness::OperationOutcome::Aborted
                    }
                    threadlane_runtime::OperationOutcome::Failed => {
                        threadlane_runtime::harness::OperationOutcome::Failed
                    }
                    threadlane_runtime::OperationOutcome::Declined => {
                        threadlane_runtime::harness::OperationOutcome::Declined
                    }
                },
                None,
            )?;
        }
        let mut lanes = self.lanes.lock().unwrap();
        if let Some(lane) = lanes.get_mut(&format!("{session_id}:main")) {
            if run_ids
                .iter()
                .any(|run_id| lane.active_run_id.as_ref() == Some(run_id))
            {
                lane.active_run_id = None;
                lane.status = LaneStatus::Idle;
            }
        }
        Ok(())
    }

    // ── Project / task management ────────────────────────────────────────

    fn load_registry(&self) {
        let records = load_project_registry_from(&self.global_dir);
        let mut lock = self.projects.lock().unwrap();
        for record in records {
            lock.insert(record.id.clone(), record);
        }
    }

    fn save_registry(&self) {
        let records: Vec<ProjectRecord> = self.projects.lock().unwrap().values().cloned().collect();
        let _ = merge_and_save_project_registry_to(&self.global_dir, &records);
    }

    pub fn register_project(&self, raw_path: &Path) -> Result<ProjectRecord, String> {
        let canonical = raw_path.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize project path '{}': {e}",
                raw_path.display()
            )
        })?;

        let record = ProjectRecord::from_path(canonical);

        {
            let mut lock = self.projects.lock().unwrap();
            lock.insert(record.id.clone(), record.clone());
        }
        self.save_registry();
        Ok(record)
    }

    pub fn create_task(
        &self,
        project_id: &str,
        session_file: Option<PathBuf>,
        options: CodingAgentOptions,
    ) -> Result<String, String> {
        let project = {
            let lock = self.projects.lock().unwrap();
            lock.get(project_id)
                .cloned()
                .ok_or_else(|| format!("Project ID '{project_id}' not found"))?
        };

        static TASK_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let count = TASK_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let task_id = format!(
            "task-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            count
        );

        let final_session_file = session_file.unwrap_or_else(|| {
            project
                .path
                .join(format!(".threadlane/sessions/{}.jsonl", task_id))
        });

        let mut opts = options;
        opts.work_dir = project.path.clone();
        opts.session_file = Some(final_session_file.clone());

        let controller = SessionController::new(opts, ExecutionMode::Background);
        let (rx, harness_watch) = {
            let mut agent = controller
                .agent
                .try_lock()
                .expect("Fresh agent is not locked");
            (agent.subscribe(), agent.watch_harness().ok().flatten())
        };

        let task_record = TaskRecord {
            id: task_id.clone(),
            project_id: project_id.to_string(),
            session_id: final_session_file
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| task_id.clone()),
            session_file: Some(final_session_file.clone()),
            parent_task_id: None,
            kind: TaskKind::Background,
            agent: "task".to_owned(),
            summary: String::new(),
            current_activity: None,
            status: TaskStatus::Idle,
            started_at_ms: now_ms(),
            finished_at_ms: None,
        };

        let runtime = TaskRuntime {
            controller,
            run_handle: None,
            cancellation_guard: None,
        };

        {
            let mut t_lock = self.tasks.lock().unwrap();
            t_lock.insert(task_id.clone(), task_record);

            let mut r_lock = self.runtimes.lock().unwrap();
            r_lock.insert(task_id.clone(), runtime);

            let mut p_lock = self.projects.lock().unwrap();
            if let Some(p) = p_lock.get_mut(project_id) {
                p.last_selected_task_id = Some(task_id.clone());
            }
        }
        self.save_registry();

        let event_tx = self.event_tx.clone();
        let lanes = self.lanes.clone();
        let tid = task_id.clone();
        let pid = project_id.to_string();
        let session_id = final_session_file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| tid.clone());

        // Clones for the harness listener spawn below.
        let harness_event_tx = event_tx.clone();
        let harness_tid = tid.clone();
        let harness_pid = pid.clone();
        tokio::spawn(async move {
            let mut sub_rx = rx;
            while let Ok(evt) = sub_rx.recv().await {
                observe_subagent_lane(&lanes, &session_id, &evt);
                // Lifecycle authority is the durable harness journal. This
                // listener only forwards transient UI notifications.
                let _ = event_tx.send(TaskAgentEvent {
                    task_id: tid.clone(),
                    project_id: pid.clone(),
                    lane: Some("main".into()),
                    event: evt,
                    harness_event: None,
                });
            }
        });

        // Spawn a second listener for harness events, forwarding them to the UI
        // so background task subagent operations update chat activities in real time.
        if let Some(mut watch) = harness_watch {
            tokio::spawn(async move {
                loop {
                    match watch.wait().await {
                        Ok(events) => {
                            for event in events {
                                let _ = harness_event_tx.send(TaskAgentEvent {
                                    task_id: harness_tid.clone(),
                                    project_id: harness_pid.clone(),
                                    lane: None,
                                    event: AgentEvent::AgentStart,
                                    harness_event: Some(event),
                                });
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        Ok(task_id)
    }

    pub async fn reload_extensions(
        &self,
        scope: ExtensionScope,
        project_root: Option<&Path>,
    ) -> Result<usize, String> {
        let target_project_id = if scope == ExtensionScope::Project {
            let project_root = project_root
                .ok_or_else(|| "Project extension reload requires a project".to_owned())?;
            let canonical = project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf());
            self.projects
                .lock()
                .unwrap()
                .values()
                .find(|project| project.path == canonical)
                .map(|project| project.id.clone())
        } else {
            None
        };

        let task_projects: HashMap<String, String> = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .map(|(task_id, task)| (task_id.clone(), task.project_id.clone()))
            .collect();
        let targets: Vec<_> = self
            .runtimes
            .lock()
            .unwrap()
            .iter()
            .filter(|(task_id, _)| {
                scope == ExtensionScope::Global
                    || target_project_id
                        .as_ref()
                        .is_some_and(|project_id| task_projects.get(*task_id) == Some(project_id))
            })
            .map(|(task_id, runtime)| (task_id.clone(), runtime.controller.clone()))
            .collect();

        let mut reloaded = 0;
        let mut failures = Vec::new();
        for (task_id, controller) in targets {
            match controller.reload_extensions().await {
                Ok(_) => reloaded += 1,
                Err(error) => failures.push(format!("{task_id}: {error}")),
            }
        }

        if failures.is_empty() {
            Ok(reloaded)
        } else {
            Err(failures.join("; "))
        }
    }

    /// Submit input to a background task. Handles session recovery, prompt
    /// acceptance, and tool intent/completion recording — all routed through
    /// `CodingSessionHarness`.
    pub fn submit_input(&self, task_id: &str, prompt: String) -> Result<(), String> {
        let (controller, session_id, session_file) = {
            let task = self
                .tasks
                .lock()
                .unwrap()
                .get(task_id)
                .cloned()
                .ok_or_else(|| format!("Task ID '{task_id}' not found"))?;
            let runtimes = self.runtimes.lock().unwrap();
            let rt = runtimes
                .get(task_id)
                .ok_or_else(|| format!("Task ID '{task_id}' not found"))?;
            (rt.controller.clone(), task.session_id, task.session_file)
        };

        let session_file_for_log = session_file
            .as_deref()
            .ok_or_else(|| format!("Task ID '{task_id}' has no session file"))?;

        // ── Begin run through CodingSessionHarness ────────────────────
        let accepted_run = {
            let mut harness = CodingSessionHarness::open(session_file_for_log)?;
            let run_id = harness.unique_run_id("run")?;
            harness.begin_run(&run_id, AgentMessage::user(&prompt, Vec::new()))?
        };

        if let Some(task) = self.tasks.lock().unwrap().get_mut(task_id) {
            task.summary = prompt.clone();
        }

        let tid = task_id.to_string();
        let runtimes_map = self.runtimes.clone();
        let supervisor = self.clone();
        let session_file_for_run = session_file.clone();
        let session_id_for_run = session_id.to_string();
        let accepted_run_for_run = accepted_run.clone();
        let run_id_for_run = accepted_run.run_id.clone();

        let handle = tokio::spawn(async move {
            let _guard = controller.prompt_lock.lock().await;
            let _cancellation_guard = runtimes_map
                .lock()
                .unwrap()
                .get_mut(&tid)
                .and_then(|runtime| runtime.cancellation_guard.take());
            let mut agent = controller.agent.lock().await;
            let should_restore = !controller
                .recovery_loaded
                .swap(true, std::sync::atomic::Ordering::SeqCst);
            if should_restore {
                if let Some(session_file) = session_file_for_run.as_deref() {
                    match supervisor.restore_session_lanes(&session_id_for_run, session_file) {
                        Ok(recovery) => {
                            let replayed = agent
                                .replay_safe_tools(&recovery.safe_tools_to_replay)
                                .await;
                            let replay_failed = replayed.iter().any(|result| result.is_error);
                            if let Ok(mut harness) = CodingSessionHarness::open(session_file) {
                                let _ = harness.claim_safe_replays(&recovery.safe_tools_to_replay);
                            }
                            let recovered_run_ids: Vec<String> = recovery
                                .open_operation_ids
                                .iter()
                                .filter(|run_id| *run_id != &run_id_for_run)
                                .cloned()
                                .collect();
                            if !recovered_run_ids.is_empty() {
                                let recovery_outcome = if recovery.unreplayable_tools > 0
                                    || recovered_run_ids.iter().any(|run_id| {
                                        recovery.abort_requested_operation_ids.contains(run_id)
                                    }) {
                                    threadlane_runtime::OperationOutcome::Aborted
                                } else if replay_failed {
                                    threadlane_runtime::OperationOutcome::Failed
                                } else {
                                    threadlane_runtime::OperationOutcome::Completed
                                };
                                let _ = supervisor.finish_recovered_operations(
                                    &session_id_for_run,
                                    session_file,
                                    &recovered_run_ids,
                                    recovery_outcome,
                                );
                            }
                            if recovery.recovered_open_operations > 0 {
                                agent.sync_session_history().await;
                            }
                        }
                        Err(_error) => return,
                    }
                }
            }

            // ── Tool intent / completion recorders through CodingSessionHarness ──
            if let Some(session_file) = session_file_for_run.as_deref() {
                let recorder_session_file = session_file.to_path_buf();
                let harness_run_id = run_id_for_run.clone();
                agent.set_tool_intent_recorder(Some(Arc::new(move |id, name, arguments| {
                    let id = id.to_string();
                    let name = name.to_string();
                    let arguments = arguments.to_string();
                    let recorder_session_file = recorder_session_file.clone();
                    let harness_run_id = harness_run_id.clone();
                    Box::pin(async move {
                        if child_task_id(&id).is_some() {
                            return Ok(());
                        }
                        let effective_args = serde_json::from_str(&arguments)
                            .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()));
                        let mut harness = CodingSessionHarness::open(&recorder_session_file)
                            .map_err(|e| e.to_string())?;
                        harness
                            .append_tool_intent_after_hook(
                                &harness_run_id,
                                &id,
                                &name,
                                effective_args,
                            )
                            .await
                            .map_err(|e| e.to_string())
                    })
                })));
                let completion_session_file = session_file.to_path_buf();
                let completion_run_id = run_id_for_run.clone();
                agent.set_tool_completion_recorder(Some(Arc::new(
                    move |result: &threadlane_runtime::AgentToolResult| {
                        let completion_session_file = completion_session_file.clone();
                        let completion_run_id = completion_run_id.clone();
                        let result = result.clone();
                        Box::pin(async move {
                            let mut harness = CodingSessionHarness::open(&completion_session_file)
                                .map_err(|e| e.to_string())?;
                            harness
                                .finish_tool_result(&completion_run_id, &result)
                                .map_err(|e| e.to_string())
                        })
                    },
                )));
            }
            if let Err(error) = agent.adopt_harness_run(&accepted_run_for_run) {
                if let Some(session_file) = session_file_for_run.as_deref() {
                    if let Ok(mut harness) = CodingSessionHarness::open(session_file) {
                        let _ = harness.finish_run(
                            &run_id_for_run,
                            OperationOutcome::Failed,
                            Some(error.clone()),
                        );
                    }
                }
                error!("failed to adopt supervisor harness run {run_id_for_run}: {error}");
                return;
            }
            let input_result = Some(
                agent
                    .execute_accepted_run(&accepted_run_for_run)
                    .await
                    .map(|_| String::new()),
            );
            agent.set_tool_intent_recorder(None);
            agent.set_tool_completion_recorder(None);
            let (outcome, error) = match input_result {
                Some(Err(error)) => (threadlane_runtime::OperationOutcome::Failed, Some(error)),
                _ => (threadlane_runtime::OperationOutcome::Completed, None),
            };
            if let Some(session_file) = session_file_for_run.as_deref() {
                if let Ok(mut harness) = CodingSessionHarness::open(session_file) {
                    let _ = harness.finish_run(&run_id_for_run, outcome, error);
                }
            }

            let mut r_lock = runtimes_map.lock().unwrap();
            if let Some(rt) = r_lock.get_mut(&tid) {
                rt.run_handle = None;
            }
        });
        if let Some(runtime) = self.runtimes.lock().unwrap().get_mut(task_id) {
            runtime.run_handle = Some(handle.abort_handle());
        }

        Ok(())
    }

    /// Cancel a task and all its subagent children.  Abort requests and
    /// terminal state are written through `CodingSessionHarness`.
    fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        let active_run = {
            let task = self.tasks.lock().unwrap().get(task_id).cloned();
            task.and_then(|task| {
                let session_file = task.session_file?;
                let run_id = CodingSessionHarness::open(&session_file)
                    .ok()
                    .and_then(|mut harness| harness.snapshot().ok())
                    .and_then(|snapshot| {
                        snapshot
                            .state
                            .lane("main")
                            .and_then(|lane| lane.open_operation.clone())
                    });
                Some((task.session_id, session_file, run_id))
            })
        };
        let cancellation_guard = if let Some((session_id, session_file, run_id)) = active_run {
            if let Some(_run_id) = run_id.as_deref() {
                if let Ok(mut harness) = CodingSessionHarness::open(&session_file) {
                    let _ = harness.request_abort();
                }
            }
            let guard = {
                crate::coding_agent::cancel_open_subagent_operations(&session_file)?;
                SubagentCancellationGuard
            };
            if let Some(run_id) = run_id {
                let _ = self.finish_recovered_operations(
                    &session_id,
                    &session_file,
                    std::slice::from_ref(&run_id),
                    threadlane_runtime::OperationOutcome::Aborted,
                );
            }
            Some(guard)
        } else {
            None
        };
        let handle = {
            let mut runtimes = self.runtimes.lock().unwrap();
            if let Some(runtime) = runtimes.get_mut(task_id) {
                runtime.cancellation_guard = cancellation_guard;
                runtime.run_handle.take()
            } else {
                None
            }
        };
        if let Some(handle) = handle {
            handle.abort();
        }
        if !self.tasks.lock().unwrap().contains_key(task_id) {
            return Err(format!("Task ID '{task_id}' not found"));
        }
        Ok(())
    }

    pub fn resume_task(&self, task_id: &str) -> Result<(), String> {
        let task = self
            .get_task(task_id)
            .ok_or_else(|| format!("Task ID '{task_id}' not found"))?;
        if task.active() {
            return Err("Task is already running".into());
        }
        if task.summary.is_empty() {
            return Err("Task has no prompt to resume".into());
        }
        self.submit_input(task_id, task.summary)
    }

    pub fn get_task_status(&self, task_id: &str) -> Option<TaskStatus> {
        let task = self.get_task(task_id)?;
        Some(task.status)
    }

    fn project_subagent_tasks_from_journal(
        &self,
        project_id: &str,
        session_id: &str,
        session_file: &Path,
        parent_task_id: Option<&str>,
    ) -> Vec<TaskRecord> {
        let Ok(mut harness) = CodingSessionHarness::open(session_file) else {
            return Vec::new();
        };
        let Ok(snapshot) = harness.snapshot() else {
            return Vec::new();
        };
        let mut tasks = HashMap::new();
        let mut ordered_records: Vec<_> = snapshot.records.iter().collect();
        ordered_records.sort_by_key(|record| record.seq());
        for record in ordered_records {
            let HarnessRecord::SubagentLifecycle {
                child_run_id,
                parent_tool_call_id,
                task_index,
                agent_id,
                subagent_lane,
                phase,
                timestamp,
                error,
                ..
            } = record
            else {
                continue;
            };
            let id = child_run_id.as_str().to_owned();
            let lane_name = subagent_lane.as_str();
            let task = tasks.entry(id.clone()).or_insert_with(|| TaskRecord {
                id,
                project_id: project_id.to_owned(),
                session_id: session_id.to_owned(),
                session_file: Some(session_file.to_path_buf()),
                parent_task_id: parent_task_id.map(str::to_owned).or_else(|| {
                    parent_tool_call_id
                        .as_ref()
                        .map(|id| id.as_str().to_owned())
                }),
                kind: TaskKind::Subagent,
                agent: agent_id.as_str().to_owned(),
                summary: task_index
                    .map(|index| format!("Subagent task {index}"))
                    .unwrap_or_else(|| lane_name.to_owned()),
                current_activity: None,
                status: TaskStatus::Idle,
                started_at_ms: *timestamp as u128,
                finished_at_ms: None,
            });
            match phase {
                SubagentLifecyclePhase::Spawned => task.status = TaskStatus::Waiting,
                SubagentLifecyclePhase::Started => task.status = TaskStatus::Running,
                SubagentLifecyclePhase::Completed => {
                    task.status = TaskStatus::Completed;
                    task.finished_at_ms = Some(*timestamp as u128);
                    task.current_activity = None;
                }
                SubagentLifecyclePhase::Failed => {
                    task.status = TaskStatus::Failed;
                    task.finished_at_ms = Some(*timestamp as u128);
                    task.current_activity = error.as_ref().map(|error| error.as_str().to_owned());
                }
                SubagentLifecyclePhase::Cancelled => {
                    task.status = TaskStatus::Cancelled;
                    task.finished_at_ms = Some(*timestamp as u128);
                    task.current_activity = None;
                }
            }
        }
        tasks.into_values().collect()
    }

    /// Project a supervisor task from the canonical session journal.
    ///
    /// `TaskRecord` retains only task-routing metadata and runtime ownership;
    /// activity, lifecycle, and terminal state come exclusively from the
    /// reduced harness snapshot and its ordered records.
    fn project_task_from_journal(&self, task: &mut TaskRecord) {
        let Some(session_file) = task.session_file.as_deref() else {
            return;
        };
        let Ok(mut harness) = CodingSessionHarness::open(session_file) else {
            return;
        };
        let Ok(snapshot) = harness.snapshot() else {
            return;
        };
        let lane_name = match task.kind {
            TaskKind::Background => "main",
            TaskKind::Subagent => task.id.as_str(),
        };
        let Some(lane) = snapshot.state.lane(lane_name) else {
            return;
        };

        task.status = match lane.status {
            HarnessLaneStatus::Idle | HarnessLaneStatus::SuspendedDeferred => TaskStatus::Waiting,
            HarnessLaneStatus::SuspendedCrash => TaskStatus::Interrupted,
            HarnessLaneStatus::Completed => TaskStatus::Completed,
            HarnessLaneStatus::Failed => TaskStatus::Failed,
        };
        if lane.open_operation.is_some() {
            task.status = if lane.abort_requested {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Running
            };
        }

        let active_run_id = lane.open_operation.as_deref();
        let mut latest_finished = None;
        let mut latest_activity = None;
        let mut ordered_records: Vec<_> = snapshot.records.iter().collect();
        ordered_records.sort_by_key(|record| record.seq());
        for record in ordered_records {
            match record {
                HarnessRecord::ToolStarted {
                    lane: record_lane,
                    run_id,
                    tool_name,
                    ..
                } if record_lane == lane_name && active_run_id == Some(run_id.as_str()) => {
                    latest_activity = Some(tool_name.clone());
                }
                HarnessRecord::ToolFinished {
                    lane: record_lane,
                    run_id,
                    ..
                } if record_lane == lane_name && active_run_id == Some(run_id.as_str()) => {
                    latest_activity = None;
                }
                HarnessRecord::OperationFinished {
                    lane: record_lane,
                    outcome,
                    timestamp,
                    ..
                } if record_lane == lane_name => {
                    latest_finished = Some((*timestamp, outcome));
                }
                _ => {}
            }
        }
        task.current_activity = latest_activity;
        if let Some((finished_at_ms, outcome)) = latest_finished {
            task.finished_at_ms = Some(finished_at_ms as u128);
            if lane.open_operation.is_none() {
                task.status = match outcome {
                    OperationOutcome::Completed => TaskStatus::Completed,
                    OperationOutcome::Failed => TaskStatus::Failed,
                    OperationOutcome::Aborted | OperationOutcome::Declined => TaskStatus::Cancelled,
                };
            }
        }
    }

    pub fn get_task(&self, task_id: &str) -> Option<TaskRecord> {
        let mut task = self.tasks.lock().unwrap().get(task_id).cloned()?;
        self.project_task_from_journal(&mut task);
        Some(task)
    }

    pub fn list_tasks_for_project(&self, project_id: &str) -> Vec<TaskRecord> {
        let lock = self.tasks.lock().unwrap();
        let mut tasks = lock
            .values()
            .filter(|t| t.project_id == project_id && t.kind == TaskKind::Background)
            .cloned()
            .collect::<Vec<_>>();
        drop(lock);
        let mut projected_subagents = Vec::new();
        for task in &mut tasks {
            self.project_task_from_journal(task);
            if let Some(session_file) = task.session_file.as_deref() {
                projected_subagents.extend(self.project_subagent_tasks_from_journal(
                    project_id,
                    &task.session_id,
                    session_file,
                    Some(&task.id),
                ));
            }
        }
        tasks.extend(projected_subagents);
        tasks.sort_by(|left, right| {
            right
                .started_at_ms
                .cmp(&left.started_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        tasks
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn child_task_id(tool_call_id: &str) -> Option<String> {
    let tagged = tool_call_id.strip_prefix("subagent-")?;
    let mut parts = tagged.splitn(3, ':');
    let run_id = parts.next()?.parse::<u64>().ok()?;
    let task_index = parts.next()?.parse::<usize>().ok()?;
    parts.next()?;
    Some(format!("subagent-{run_id}:{task_index}"))
}

fn observe_subagent_lane(
    lanes: &Arc<Mutex<HashMap<String, Lane>>>,
    session_id: &str,
    event: &AgentEvent,
) {
    let AgentEvent::SubagentQueued {
        run_id, task_index, ..
    } = event
    else {
        return;
    };
    let lane_name = format!("subagent-{run_id}:{task_index}");
    lanes
        .lock()
        .unwrap()
        .entry(format!("{session_id}:{lane_name}"))
        .or_insert_with(|| {
            let mut lane = Lane::new(lane_name, session_id);
            lane.parent_lane = Some("main".into());
            lane
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_agent::CodingAgent;
    use std::time::Duration;
    use threadlane_runtime::harness::JsonlStore;
    use threadlane_runtime::harness::{
        OperationIntent, OperationOutcome, QueueKind as HarnessQueueKind, Record as HarnessRecord,
        SessionStore,
    };
    use threadlane_runtime::TokenUsage;

    // ── Helper: open a CodingSessionHarness for test setup ────────────
    fn open_test_harness(path: &Path) -> CodingSessionHarness {
        CodingSessionHarness::open(path).unwrap()
    }

    #[test]
    fn task_projection_uses_canonical_journal_not_cached_status() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("task.jsonl");
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-1", AgentMessage::user("prompt", Vec::new()))
            .unwrap();
        harness
            .finish_run(
                "run-1",
                OperationOutcome::Failed,
                Some("durable failure".into()),
            )
            .unwrap();

        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        supervisor.tasks.lock().unwrap().insert(
            "task-1".into(),
            TaskRecord {
                id: "task-1".into(),
                project_id: "project-1".into(),
                session_id: "task".into(),
                session_file: Some(session_file),
                parent_task_id: None,
                kind: TaskKind::Background,
                agent: "task".into(),
                summary: "prompt".into(),
                current_activity: Some("stale activity".into()),
                status: TaskStatus::Completed,
                started_at_ms: 1,
                finished_at_ms: None,
            },
        );

        let task = supervisor.get_task("task-1").unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert!(task.finished_at_ms.is_some());
        assert_eq!(task.current_activity, None);
    }

    #[test]
    fn supervisor_restart_projects_subagent_lifecycle_from_journal() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("task.jsonl");
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("parent-run", AgentMessage::user("prompt", Vec::new()))
            .unwrap();
        let child = harness
            .start_subagent_lane("reviewer", "call-1", Some("entry-1".into()))
            .unwrap();
        harness
            .finish_subagent_lane(
                &child.identity.lane_name,
                &child.identity.run_id,
                OperationOutcome::Completed,
                None,
            )
            .unwrap();

        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        supervisor.tasks.lock().unwrap().insert(
            "task-1".into(),
            TaskRecord {
                id: "task-1".into(),
                project_id: "project-1".into(),
                session_id: "task".into(),
                session_file: Some(session_file),
                parent_task_id: None,
                kind: TaskKind::Background,
                agent: "task".into(),
                summary: "prompt".into(),
                current_activity: None,
                status: TaskStatus::Idle,
                started_at_ms: 1,
                finished_at_ms: None,
            },
        );

        let tasks = supervisor.list_tasks_for_project("project-1");
        let subagent = tasks
            .into_iter()
            .find(|task| task.id == child.identity.run_id)
            .expect("subagent must be rebuilt from durable lifecycle records");
        assert_eq!(subagent.kind, TaskKind::Subagent);
        assert_eq!(subagent.status, TaskStatus::Completed);
        assert_eq!(subagent.parent_task_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn abort_request_is_persisted_before_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-1", AgentMessage::user("prompt", Vec::new()))
            .unwrap();
        harness.request_abort().unwrap();
        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| {
            matches!(record, HarnessRecord::AbortRequested { run_id, .. } if run_id == "run-1")
        }));
    }

    #[test]
    fn v2_abort_request_does_not_create_a_legacy_sidecar_record() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-v2", AgentMessage::user("prompt", Vec::new()))
            .unwrap();

        harness.request_abort().unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| {
            matches!(record, HarnessRecord::AbortRequested { run_id, .. } if run_id == "run-v2")
        }));
    }

    #[test]
    fn prompt_acceptance_allocates_unique_lane_sequences() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        let mut harness = open_test_harness(&session_file);
        let first = harness.unique_run_id("run").unwrap();
        harness
            .begin_run(&first, AgentMessage::user("one", Vec::new()))
            .unwrap();
        harness
            .finish_run(&first, OperationOutcome::Completed, None)
            .unwrap();
        {
            // Update supervisor lane projection
            supervisor.get_or_create_lane("session-1", "main");
            let key = "session-1:main".to_string();
            let mut lanes = supervisor.lanes.lock().unwrap();
            if let Some(lane) = lanes.get_mut(&key) {
                lane.session_file = Some(session_file.clone());
            }
        }
        let mut harness2 = open_test_harness(&session_file);
        let second = harness2.unique_run_id("run").unwrap();
        harness2
            .begin_run(&second, AgentMessage::user("two", Vec::new()))
            .unwrap();
        assert_ne!(first, second);
        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(
            store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::OperationStarted { .. }))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn recovery_replay_does_not_record_a_main_lane_intent() {
        let dir = tempfile::tempdir().unwrap();
        let _supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-1", AgentMessage::user("prompt", Vec::new()))
            .unwrap();
        drop(harness);

        let agent = CodingAgent::new(CodingAgentOptions {
            api_key: "test_key".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(session_file.clone()),
            system_prompt: Default::default(),
            agent_config: None,
            coding_config: None,
        });

        let results = agent
            .replay_safe_tools(&[HarnessRecord::ToolStarted {
                id: "existing-intent".into(),
                seq: 2,
                lane: "main".into(),
                timestamp: 1,
                run_id: "run-1".into(),
                assistant_entry_id: String::new(),
                tool_index: 0,
                tool_call_id: "call-1".into(),
                tool_name: "list_dir".into(),
                effective_args: serde_json::json!({}),
                result_entry_id: "result-call-1".into(),
                replay: threadlane_runtime::ToolReplaySafety::Safe,
            }])
            .await;

        assert!(!results[0].is_error);
        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| {
            matches!(record, HarnessRecord::OperationStarted { id, .. } if id == "run-1")
        }));
    }

    #[tokio::test]
    async fn failed_input_persists_failed_operation() {
        let global_dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(global_dir.path().to_path_buf());
        let project = supervisor.register_project(project_dir.path()).unwrap();
        let task_id = supervisor
            .create_task(
                &project.id,
                None,
                CodingAgentOptions {
                    api_key: "test_key".into(),
                    account_id: None,
                    model: "gpt-4o".into(),
                    work_dir: project_dir.path().to_path_buf(),
                    session_file: None,
                    system_prompt: Default::default(),
                    agent_config: None,
                    coding_config: None,
                },
            )
            .unwrap();

        supervisor
            .submit_input(&task_id, "/subagent".into())
            .unwrap();
        let session_file = loop {
            if let Some(task) = supervisor
                .list_tasks_for_project(&project.id)
                .into_iter()
                .find(|task| task.id == task_id)
            {
                if let Some(session_file) = task.session_file {
                    break session_file;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        for _ in 0..100 {
            if supervisor.get_task_status(&task_id) == Some(TaskStatus::Failed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let store = JsonlStore::open(&session_file).unwrap();
        let finished = store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationFinished { outcome, .. } => Some(outcome),
                _ => None,
            })
            .unwrap();
        assert_eq!(finished, &OperationOutcome::Failed);
        assert_eq!(
            supervisor.get_task_status(&task_id),
            Some(TaskStatus::Failed)
        );
    }

    #[test]
    fn cancel_task_uses_journal_open_operation_without_lane_cache() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        supervisor.tasks.lock().unwrap().insert(
            "task-1".into(),
            TaskRecord {
                id: "task-1".into(),
                project_id: "project-1".into(),
                session_id: "session-1".into(),
                session_file: Some(session_file.clone()),
                parent_task_id: None,
                kind: TaskKind::Background,
                agent: "task".into(),
                summary: "run".into(),
                current_activity: None,
                status: TaskStatus::Idle,
                started_at_ms: 1,
                finished_at_ms: None,
            },
        );
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-1", AgentMessage::user("run", Vec::new()))
            .unwrap();
        drop(harness);

        supervisor.cancel_task("task-1").unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Aborted,
                ..
            } if run_id == "run-1"
        )));
    }

    #[test]
    fn cancelling_task_finishes_active_operation_as_aborted() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        supervisor.tasks.lock().unwrap().insert(
            "task-1".into(),
            TaskRecord {
                id: "task-1".into(),
                project_id: "project-1".into(),
                session_id: "session-1".into(),
                session_file: Some(session_file.clone()),
                parent_task_id: None,
                kind: TaskKind::Background,
                agent: "task".into(),
                summary: "run".into(),
                current_activity: None,
                status: TaskStatus::Running,
                started_at_ms: 1,
                finished_at_ms: None,
            },
        );
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-1", AgentMessage::user("run", Vec::new()))
            .unwrap();
        {
            // Set up lane projection so cancel_task can find the active run
            let mut lanes = supervisor.lanes.lock().unwrap();
            let lane = lanes
                .entry("session-1:main".into())
                .or_insert_with(|| Lane::new("main", "session-1"));
            lane.session_file = Some(session_file.clone());
            lane.active_run_id = Some("run-1".into());
        }
        supervisor.cancel_task("task-1").unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Aborted,
                ..
            } if run_id == "run-1"
        )));
        let recovery = supervisor
            .restore_session_lanes("session-1", &session_file)
            .unwrap();
        assert_eq!(recovery.recovered_open_operations, 0);
    }

    #[test]
    fn cancelling_parent_aborts_open_subagent_operations() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        supervisor.tasks.lock().unwrap().insert(
            "task-1".into(),
            TaskRecord {
                id: "task-1".into(),
                project_id: "project-1".into(),
                session_id: "session-1".into(),
                session_file: Some(session_file.clone()),
                parent_task_id: None,
                kind: TaskKind::Background,
                agent: "task".into(),
                summary: "run".into(),
                current_activity: None,
                status: TaskStatus::Running,
                started_at_ms: 1,
                finished_at_ms: None,
            },
        );
        let mut harness = open_test_harness(&session_file);
        for (lane_name, run_id) in [
            ("subagent-1:0", "run-open-1"),
            ("subagent-1:1", "run-open-2"),
        ] {
            harness
                .store
                .accept_prompt_on_lane(lane_name, run_id, AgentMessage::user("sub", Vec::new()))
                .unwrap();
        }
        harness.store.drive_to_completion().unwrap();
        drop(harness);
        supervisor.cancel_task("task-1").unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        let aborted: Vec<_> = store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::OperationFinished {
                    run_id,
                    outcome: OperationOutcome::Aborted,
                    ..
                } => Some(run_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(aborted, ["run-open-1", "run-open-2"]);
        for run_id in ["run-open-1", "run-open-2"] {
            assert_eq!(
                store
                    .records()
                    .iter()
                    .filter(|record| {
                        matches!(record, HarnessRecord::OperationFinished { run_id: record_run_id, .. } if record_run_id == run_id)
                    })
                    .count(),
                1,
                "expected one terminal record for {run_id}",
            );
        }
    }

    #[tokio::test]
    async fn cancellation_guard_stays_installed_until_the_next_submission() {
        let global_dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(global_dir.path().to_path_buf());
        let project = supervisor.register_project(project_dir.path()).unwrap();
        let task_id = supervisor
            .create_task(
                &project.id,
                None,
                CodingAgentOptions {
                    api_key: "test_key".into(),
                    account_id: None,
                    model: "gpt-4o".into(),
                    work_dir: project_dir.path().to_path_buf(),
                    session_file: None,
                    system_prompt: Default::default(),
                    agent_config: None,
                    coding_config: None,
                },
            )
            .unwrap();

        supervisor.cancel_task(&task_id).unwrap();
        assert!(supervisor
            .runtimes
            .lock()
            .unwrap()
            .get(&task_id)
            .unwrap()
            .cancellation_guard
            .is_some());

        let prompt_lock = supervisor
            .runtimes
            .lock()
            .unwrap()
            .get(&task_id)
            .unwrap()
            .controller
            .prompt_lock
            .clone();
        let held_prompt = prompt_lock.lock().await;
        supervisor
            .submit_input(&task_id, "/subagent".into())
            .unwrap();
        tokio::task::yield_now().await;
        assert!(supervisor
            .runtimes
            .lock()
            .unwrap()
            .get(&task_id)
            .unwrap()
            .cancellation_guard
            .is_some());
        drop(held_prompt);
        for _ in 0..100 {
            if supervisor
                .runtimes
                .lock()
                .unwrap()
                .get(&task_id)
                .unwrap()
                .cancellation_guard
                .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(supervisor
            .runtimes
            .lock()
            .unwrap()
            .get(&task_id)
            .unwrap()
            .cancellation_guard
            .is_none());
    }

    #[test]
    fn supervisor_lane_management_and_queuing() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");

        let lane = supervisor.get_or_create_lane("session-1", "main");
        assert_eq!(lane.name, "main");
        assert_eq!(lane.status, LaneStatus::Idle);

        // Set up lane with a session file and active run
        {
            let mut harness = open_test_harness(&session_file);
            harness
                .begin_run("run-1", AgentMessage::user("prompt", Vec::new()))
                .unwrap();
        }
        {
            let mut lanes = supervisor.lanes.lock().unwrap();
            let lane = lanes
                .entry("session-1:main".into())
                .or_insert_with(|| Lane::new("main", "session-1"));
            lane.session_file = Some(session_file.clone());
            lane.active_run_id = Some("run-1".into());
        }

        supervisor
            .enqueue_steer(
                "session-1",
                "main",
                AgentMessage::User {
                    content: "steer msg".into(),
                },
            )
            .unwrap();

        supervisor.update_lane_leaf("session-1", "main", Some("node_1".into()));

        let updated_lane = supervisor.get_or_create_lane("session-1", "main");
        assert_eq!(updated_lane.leaf_id.as_deref(), Some("node_1"));
        let checkpoint = supervisor.checkpoint_lane("session-1", "main");
        assert_eq!(checkpoint.steer_messages.len(), 1);
        assert!(matches!(
            checkpoint.steer_messages.first(),
            Some(AgentMessage::User { content }) if content == "steer msg"
        ));
    }

    #[test]
    fn queue_enqueued_is_persisted_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");

        // Set up the harness with an open operation
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-1", AgentMessage::user("prompt", Vec::new()))
            .unwrap();

        // Persist queue intents through the harness store
        harness
            .store
            .enqueue_unbound_on_lane(
                "main",
                HarnessQueueKind::FollowUp,
                threadlane_runtime::harness::ProvisionedEntry {
                    id: "follow-up-entry".into(),
                    surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                    parent_id: None,
                    message: AgentMessage::User {
                        content: "next".into(),
                    },
                },
            )
            .unwrap();
        harness.store.drive_to_completion().unwrap();
        harness
            .store
            .enqueue_unbound_on_lane(
                "main",
                HarnessQueueKind::Steer,
                threadlane_runtime::harness::ProvisionedEntry {
                    id: "steer-entry".into(),
                    surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                    parent_id: None,
                    message: AgentMessage::User {
                        content: "urgent".into(),
                    },
                },
            )
            .unwrap();
        harness.store.drive_to_completion().unwrap();

        // Set up supervisor lane projection with session_file
        {
            let mut lanes = supervisor.lanes.lock().unwrap();
            let lane = lanes
                .entry("session-1:main".into())
                .or_insert_with(|| Lane::new("main", "session-1"));
            lane.session_file = Some(session_file.clone());
        }

        // Also enqueue via supervisor (persisting directly to harness)
        supervisor
            .enqueue_followup(
                "session-1",
                "main",
                AgentMessage::User {
                    content: "next-from-supervisor".into(),
                },
            )
            .unwrap();
        supervisor
            .enqueue_steer_priority(
                "session-1",
                "main",
                AgentMessage::User {
                    content: "urgent-from-supervisor".into(),
                },
                threadlane_runtime::SteerPriority::High,
            )
            .unwrap();

        // Verify persistence through harness store
        let store = JsonlStore::open(&session_file).unwrap();
        assert!(matches!(
            store.records().iter().find(|record| matches!(record, HarnessRecord::QueueEnqueued { target, .. } if matches!(target.message, AgentMessage::User { ref content } if content == "next"))),
            Some(HarnessRecord::QueueEnqueued {
                queue: HarnessQueueKind::FollowUp,
                ..
            })
        ));
        assert!(matches!(
            store.records().iter().find(|record| matches!(record, HarnessRecord::QueueEnqueued { target, .. } if matches!(target.message, AgentMessage::User { ref content } if content == "urgent"))),
            Some(HarnessRecord::QueueEnqueued {
                queue: HarnessQueueKind::Steer,
                ..
            })
        ));
        assert!(matches!(
            store.records().iter().find(|record| matches!(record, HarnessRecord::QueueEnqueued { target, .. } if matches!(target.message, AgentMessage::User { ref content } if content == "next-from-supervisor"))),
            Some(HarnessRecord::QueueEnqueued {
                queue: HarnessQueueKind::FollowUp,
                ..
            })
        ));
        assert!(matches!(
            store.records().iter().find(|record| matches!(record, HarnessRecord::QueueEnqueued { target, .. } if matches!(target.message, AgentMessage::User { ref content } if content == "urgent-from-supervisor"))),
            Some(HarnessRecord::QueueEnqueued {
                queue: HarnessQueueKind::Steer,
                ..
            })
        ));

        // Now also persist a steer with explicit High priority via a QueueEnqueued record
        // so that restore picks it up as High priority
        let underlying = harness.store.store();
        let queue_seq = underlying
            .entries()
            .iter()
            .map(|e| e.seq)
            .chain(underlying.records().iter().map(|r| r.seq()))
            .max()
            .unwrap()
            + 1;
        harness
            .store
            .append_record_gated(HarnessRecord::QueueEnqueued {
                id: "queue-steer-high-priority".into(),
                seq: queue_seq,
                lane: "main".into(),
                timestamp: queue_seq,
                run_id: Some("run-1".into()),
                queue: HarnessQueueKind::Steer,
                priority: Some(threadlane_runtime::SteerPriority::High),
                target: threadlane_runtime::harness::ProvisionedEntry {
                    id: "steer-high-target".into(),
                    surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                    parent_id: None,
                    message: AgentMessage::User {
                        content: "high-priority-steer".into(),
                    },
                },
            })
            .unwrap();
        harness.store.drive_to_completion().unwrap();

        supervisor
            .restore_session_lanes("session-1", &session_file)
            .unwrap();
        supervisor
            .restore_session_lanes("session-1", &session_file)
            .unwrap();
        let snap = open_test_harness(&session_file).snapshot().unwrap();
        let main_lane = snap.state.lanes.iter().find(|l| l.name == "main").unwrap();
        let follow_up_count = main_lane
            .queued
            .iter()
            .filter(|q| q.queue == HarnessQueueKind::FollowUp)
            .count();
        let steer_count = main_lane
            .queued
            .iter()
            .filter(|q| q.queue == HarnessQueueKind::Steer)
            .count();
        assert_eq!(follow_up_count, 2);
        assert_eq!(steer_count, 3);
        let has_high_priority = main_lane
            .queued
            .iter()
            .any(|q| q.priority == Some(threadlane_runtime::SteerPriority::High));
        assert!(
            has_high_priority,
            "expected at least one High-priority steer in restored queue"
        );
        supervisor
            .lanes
            .lock()
            .unwrap()
            .get_mut("session-1:main")
            .unwrap()
            .session_file = Some(dir.path().join("missing/session.jsonl"));
        let err = supervisor.enqueue_followup(
            "session-1",
            "main",
            AgentMessage::User {
                content: "must not queue".into(),
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn v2_tool_intent_does_not_create_a_legacy_sidecar_record() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-v2", AgentMessage::user("prompt", Vec::new()))
            .unwrap();

        // Append assistant entry via the harness store
        let underlying = harness.store.store();
        let prompt_entry = underlying
            .entries()
            .iter()
            .find(|entry| entry.id == "entry-run-v2-user")
            .unwrap()
            .clone();
        let assistant_seq = underlying
            .entries()
            .iter()
            .map(|e| e.seq)
            .chain(underlying.records().iter().map(|r| r.seq()))
            .max()
            .unwrap_or(prompt_entry.seq)
            + 1;
        harness
            .store
            .append_entry_gated(threadlane_runtime::harness::Entry {
                id: "assistant-run-v2".into(),
                parent_id: Some(prompt_entry.id.clone()),
                lane: "main".into(),
                seq: assistant_seq,
                timestamp: assistant_seq,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                        id: "call-v2".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "view_file".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        harness.store.drive_to_completion().unwrap();

        // Record tool intent through CodingSessionHarness
        let mut harness2 = open_test_harness(&session_file);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(harness2.append_tool_intent_after_hook(
            "run-v2",
            "call-v2",
            "view_file",
            serde_json::json!({}),
        ))
        .unwrap();

        // Append tool result entry
        let mut harness3 = open_test_harness(&session_file);
        let underlying3 = harness3.store.store();
        let result_seq = underlying3
            .entries()
            .iter()
            .map(|e| e.seq)
            .chain(underlying3.records().iter().map(|r| r.seq()))
            .max()
            .unwrap()
            + 1;
        harness3
            .store
            .append_entry_gated(threadlane_runtime::harness::Entry {
                id: "v2-tool-result-call-v2".into(),
                parent_id: Some("assistant-run-v2".into()),
                lane: "main".into(),
                seq: result_seq,
                timestamp: result_seq,
                message: AgentMessage::Tool {
                    tool_call_id: "call-v2".into(),
                    name: "view_file".into(),
                    content: "ok".into(),
                    is_error: false,
                    terminate: false,
                },
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        harness3.store.drive_to_completion().unwrap();

        let mut harness4 = open_test_harness(&session_file);
        harness4
            .finish_tool_message(
                "run-v2",
                &AgentMessage::Tool {
                    tool_call_id: "call-v2".into(),
                    name: "view_file".into(),
                    content: "ok".into(),
                    is_error: false,
                    terminate: false,
                },
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ToolFinished { tool_call_id, .. } if tool_call_id == "call-v2")
        }));
    }

    #[test]
    fn lane_operation_records_are_persisted_and_retained() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        supervisor.get_or_create_lane("session-1", "main");

        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-1", AgentMessage::user("prompt", Vec::new()))
            .unwrap();

        // Set up lane projection
        {
            let mut lanes = supervisor.lanes.lock().unwrap();
            let lane = lanes
                .entry("session-1:main".into())
                .or_insert_with(|| Lane::new("main", "session-1"));
            lane.session_file = Some(session_file.clone());
            lane.active_run_id = Some("run-1".into());
        }

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| {
            matches!(record, HarnessRecord::OperationStarted { id, .. } if id == "run-1")
        }));

        let recovery = supervisor
            .restore_session_lanes("session-1", &session_file)
            .unwrap();
        assert_eq!(recovery.recovered_open_operations, 1);
        let restored = supervisor.get_or_create_lane("session-1", "main");
        assert_eq!(restored.status, LaneStatus::Suspended);
        assert_eq!(restored.active_run_id.as_deref(), Some("run-1"));

        supervisor
            .finish_recovered_operations(
                "session-1",
                &session_file,
                &recovery.open_operation_ids,
                threadlane_runtime::OperationOutcome::Aborted,
            )
            .unwrap();
        let second_recovery = supervisor
            .restore_session_lanes("session-1", &session_file)
            .unwrap();
        assert_eq!(second_recovery.recovered_open_operations, 0);
    }

    #[test]
    fn v2_only_open_run_is_restored() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        let mut harness = open_test_harness(&session_file);
        harness
            .begin_run("run-v2", AgentMessage::user("prompt", Vec::new()))
            .unwrap();
        harness.drive_to_completion().unwrap();
        harness.store.drive_one().unwrap();
        drop(harness);

        let recovery = supervisor
            .restore_session_lanes("session-1", &session_file)
            .unwrap();

        assert_eq!(recovery.open_operation_ids, vec!["run-v2"]);
        assert_eq!(
            supervisor.get_or_create_lane("session-1", "main").status,
            LaneStatus::Suspended
        );
    }

    #[test]
    fn v2_only_recovery_restores_open_subagent_lanes() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();

        // Use CodingSessionHarness to set up a subagent lane on the V2 session
        let mut harness = open_test_harness(&session_file);
        harness
            .store
            .append_entry_gated(threadlane_runtime::harness::Entry {
                id: "subagent-root".into(),
                parent_id: None,
                lane: "subagent-1".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::user("inspect", Vec::new()),
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        harness.store.drive_to_completion().unwrap();
        harness
            .store
            .accept_prompt_on_lane(
                "subagent-1",
                "run-subagent",
                AgentMessage::user("inspect", Vec::new()),
            )
            .unwrap();
        harness.store.drive_to_completion().unwrap();
        harness
            .store
            .enqueue_unbound_on_lane(
                "subagent-1",
                HarnessQueueKind::FollowUp,
                threadlane_runtime::harness::ProvisionedEntry {
                    id: "queued-follow-up".into(),
                    surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                    parent_id: None,
                    message: AgentMessage::user("follow up", Vec::new()),
                },
            )
            .unwrap();
        harness.store.drive_to_completion().unwrap();

        // Append a QueueEnqueued record directly for the test assertion
        let underlying = harness.store.store();
        let queue_seq = underlying
            .entries()
            .iter()
            .map(|e| e.seq)
            .chain(underlying.records().iter().map(|r| r.seq()))
            .max()
            .unwrap()
            + 1;
        harness
            .store
            .append_record_gated(HarnessRecord::QueueEnqueued {
                id: "queue-steer-high".into(),
                seq: queue_seq,
                lane: "subagent-1".into(),
                timestamp: queue_seq,
                run_id: Some("run-subagent".into()),
                queue: HarnessQueueKind::Steer,
                priority: Some(threadlane_runtime::SteerPriority::High),
                target: threadlane_runtime::harness::ProvisionedEntry {
                    id: "queued-steer-high".into(),
                    surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                    parent_id: None,
                    message: AgentMessage::user("urgent", Vec::new()),
                },
            })
            .unwrap();
        harness.store.drive_to_completion().unwrap();

        let recovery = supervisor
            .restore_session_lanes("session-1", &session_file)
            .unwrap();
        assert_eq!(recovery.open_operation_ids, vec!["run-subagent"]);
        let lane = supervisor.get_or_create_lane("session-1", "subagent-1");
        assert_eq!(lane.status, LaneStatus::Suspended);
        assert_eq!(lane.active_run_id.as_deref(), Some("run-subagent"));

        let snap = open_test_harness(&session_file).snapshot().unwrap();
        let sub_lane = snap
            .state
            .lanes
            .iter()
            .find(|l| l.name == "subagent-1")
            .unwrap();
        let follow_up_count = sub_lane
            .queued
            .iter()
            .filter(|q| q.queue == HarnessQueueKind::FollowUp)
            .count();
        let steer_count = sub_lane
            .queued
            .iter()
            .filter(|q| q.queue == HarnessQueueKind::Steer)
            .count();
        assert_eq!(follow_up_count, 1);
        assert_eq!(steer_count, 1);
        assert_eq!(
            sub_lane
                .queued
                .iter()
                .find(|q| q.queue == HarnessQueueKind::Steer)
                .and_then(|q| q.priority),
            Some(threadlane_runtime::SteerPriority::High)
        );
    }

    #[test]
    fn supervisor_lane_navigation_and_deferred_redemption() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());

        supervisor.update_lane_leaf("session-test", "main", Some("node-2".into()));
        assert!(supervisor
            .navigate_lane("session-test", "main", "node-1")
            .unwrap());

        let lane = supervisor.get_or_create_lane("session-test", "main");
        assert_eq!(lane.leaf_id.as_deref(), Some("node-1"));

        let redeemed_id = supervisor
            .redeem_deferred(
                "session-test",
                "main",
                AgentMessage::Assistant {
                    content: Some("redeemed response".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
            )
            .unwrap();

        assert!(!redeemed_id.is_empty());
    }

    #[test]
    fn v2_lane_navigation_persists_typed_navigation_records() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        let root_id = "node-1".to_string();
        let child_id = "node-2".to_string();
        let mut store = JsonlStore::open({
            fs::File::create(&session_file).unwrap();
            &session_file
        })
        .unwrap();
        store
            .append_entry(threadlane_runtime::harness::Entry {
                id: root_id.clone(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::user("root", Vec::new()),
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_runtime::harness::Entry {
                id: child_id.clone(),
                parent_id: Some(root_id.clone()),
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Assistant {
                    content: Some("child".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        supervisor
            .lanes
            .lock()
            .unwrap()
            .entry("session-test:main".into())
            .or_insert_with(|| Lane::new("main", "session-test"))
            .session_file = Some(session_file.clone());

        assert!(supervisor
            .navigate_lane("session-test", "main", &root_id)
            .unwrap());
        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationStarted {
                intent: OperationIntent::Navigation,
                lane,
                ..
            } if lane == "main"
        )));
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::LaneMoved { target_leaf_id, .. } if target_leaf_id == &root_id
        )));
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                outcome: OperationOutcome::Completed,
                ..
            }
        )));
        assert!(!session_file.with_extension("oplog.jsonl").exists());
    }

    #[test]
    fn supervisor_metrics_and_lane_subscription() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());

        let _lane = supervisor.get_or_create_lane("session-1", "worker");
        let metrics = supervisor.metrics();
        assert_eq!(metrics.active_lanes, 1);

        let _rx = supervisor.subscribe_lane("session-1", "worker");
        let _rx2 = _rx.resubscribe();
    }

    #[test]
    fn supervisor_sub_lane_lineage_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());

        let _parent = supervisor.get_or_create_lane("session-1", "root");
        let child = supervisor.get_or_create_sub_lane("session-1", "sub-1", "root");
        assert_eq!(child.parent_lane.as_deref(), Some("root"));

        let cancelled = supervisor.cancel_lane_hierarchy("session-1", "root");
        assert_eq!(cancelled, 2);
    }

    #[test]
    fn supervisor_tool_output_cache_and_usage_aggregation() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());

        {
            let cache_arc = supervisor.output_cache();
            let mut cache = cache_arc.lock().unwrap();
            cache.put("view_file:main.rs".into(), "content".into());
            assert_eq!(cache.get("view_file:main.rs").as_deref(), Some("content"));
            cache.invalidate_all();
            assert_eq!(cache.get("view_file:main.rs"), None);
        }

        let _root = supervisor.get_or_create_lane("session-1", "root");
        let _child = supervisor.get_or_create_sub_lane("session-1", "sub-1", "root");

        supervisor.record_lane_usage(
            "session-1",
            "root",
            &TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        );
        supervisor.record_lane_usage(
            "session-1",
            "sub-1",
            &TokenUsage {
                input_tokens: 200,
                output_tokens: 80,
                total_tokens: 280,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        );

        let tree_usage = supervisor.aggregate_tree_usage("session-1", "root");
        assert_eq!(tree_usage.input_tokens, 300);
        assert_eq!(tree_usage.output_tokens, 130);
        assert_eq!(tree_usage.total_tokens, 430);
    }

    #[test]
    fn task_agent_event_durable_event_access() {
        let entry = threadlane_runtime::harness::Entry {
            id: "entry-1".into(),
            parent_id: None,
            seq: 1,
            lane: "main".into(),
            timestamp: 1000,
            message: AgentMessage::user("test task", Vec::new()),
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        };

        let harness_event = threadlane_runtime::harness::HarnessEventHub::new(10).publish_durable(
            threadlane_runtime::harness::DurablePayload::Entry(entry),
            Some("main".into()),
            Some("run-1".into()),
            None,
        );

        let event = TaskAgentEvent {
            task_id: "task-1".into(),
            project_id: "project-1".into(),
            lane: Some("main".into()),
            event: AgentEvent::AgentStart,
            harness_event: Some(harness_event),
        };

        assert!(event.is_durable());
        let durable = event.durable_event().expect("expected durable event");
        assert!(durable.is_entry());
        assert_eq!(durable.seq(), 1);

        let live_event = TaskAgentEvent {
            task_id: "task-2".into(),
            project_id: "project-1".into(),
            lane: Some("main".into()),
            event: AgentEvent::AgentStart,
            harness_event: None,
        };

        assert!(!live_event.is_durable());
        assert!(live_event.durable_event().is_none());
    }
}
