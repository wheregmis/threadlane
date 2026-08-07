use crate::coding_agent::{
    abort_open_subagent_operations, CodingAgent, CodingAgentOptions, SubagentCancellationGuard,
};
use crate::packages::ExtensionScope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use threadlane_agent::harness::{
    AgentHarness, JsonlStore, OperationIntent, OperationOutcome, ProvisionedEntry,
    QueueKind as HarnessQueueKind, Record as HarnessRecord, Reducer, SessionStore,
};
use threadlane_agent::{AgentEvent, AgentMessage, LaneQueue, OpRecord, QueueKind, TokenUsage};

fn append_v2_lifecycle_record(session_file: &Path, record: HarnessRecord) -> Result<(), String> {
    if let Some(parent) = session_file.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if !session_file.exists() {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(session_file)
            .map_err(|error| error.to_string())?;
    }
    let mut store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
    if store
        .records()
        .iter()
        .any(|current| current.id() == record.id())
    {
        return Ok(());
    }
    let seq = store
        .entries()
        .iter()
        .map(|entry| entry.seq)
        .chain(store.records().iter().map(HarnessRecord::seq))
        .max()
        .unwrap_or(0)
        + 1;
    let record = match record {
        HarnessRecord::OperationStarted {
            id,
            lane,
            timestamp,
            source_leaf_id,
            intent,
            ..
        } => HarnessRecord::OperationStarted {
            id,
            seq,
            lane,
            timestamp,
            source_leaf_id,
            intent,
        },
        HarnessRecord::OperationFinished {
            id,
            lane,
            timestamp,
            run_id,
            outcome,
            error,
            ..
        } => HarnessRecord::OperationFinished {
            id,
            seq,
            lane,
            timestamp,
            run_id,
            outcome,
            error,
        },
        HarnessRecord::AbortRequested {
            id,
            lane,
            timestamp,
            run_id,
            ..
        } => HarnessRecord::AbortRequested {
            id,
            seq,
            lane,
            timestamp,
            run_id,
        },
        HarnessRecord::QueueEnqueued {
            id,
            lane,
            timestamp,
            run_id,
            queue,
            priority,
            target,
            ..
        } => HarnessRecord::QueueEnqueued {
            id,
            seq,
            lane,
            timestamp,
            run_id,
            queue,
            priority,
            target,
        },
        HarnessRecord::ToolStarted {
            id,
            lane,
            timestamp,
            run_id,
            assistant_entry_id,
            tool_index,
            tool_call_id,
            tool_name,
            effective_args,
            result_entry_id,
            replay,
            ..
        } => HarnessRecord::ToolStarted {
            id,
            seq,
            lane,
            timestamp,
            run_id,
            assistant_entry_id,
            tool_index,
            tool_call_id,
            tool_name,
            effective_args,
            result_entry_id,
            replay,
        },
        HarnessRecord::ToolFinished {
            id,
            lane,
            timestamp,
            run_id,
            tool_call_id,
            result_entry_id,
            terminate,
            ..
        } => HarnessRecord::ToolFinished {
            id,
            seq,
            lane,
            timestamp,
            run_id,
            tool_call_id,
            result_entry_id,
            terminate,
        },
        _ => return Err("unsupported V2 supervisor lifecycle record".into()),
    };
    store
        .append_record(record)
        .map_err(|error| error.to_string())
}

fn v2_lifecycle_from_op_record(record: &OpRecord) -> Option<HarnessRecord> {
    match record {
        OpRecord::OperationStarted {
            id,
            lane,
            timestamp,
            source_leaf_id,
            ..
        } => Some(HarnessRecord::OperationStarted {
            id: id.clone(),
            seq: 0,
            lane: lane.clone(),
            timestamp: *timestamp,
            source_leaf_id: source_leaf_id.clone(),
            intent: OperationIntent::Run,
        }),
        OpRecord::OperationFinished {
            id,
            lane,
            timestamp,
            run_id,
            outcome,
            error,
            ..
        } => Some(HarnessRecord::OperationFinished {
            id: format!("v2-{id}"),
            seq: 0,
            lane: lane.clone(),
            timestamp: *timestamp,
            run_id: run_id.clone(),
            outcome: match outcome {
                threadlane_agent::OpOutcome::Completed => OperationOutcome::Completed,
                threadlane_agent::OpOutcome::Aborted => OperationOutcome::Aborted,
                threadlane_agent::OpOutcome::Failed => OperationOutcome::Failed,
                threadlane_agent::OpOutcome::Declined => OperationOutcome::Declined,
            },
            error: error.clone(),
        }),
        OpRecord::AbortRequested {
            id,
            lane,
            timestamp,
            run_id,
            ..
        } => Some(HarnessRecord::AbortRequested {
            id: format!("v2-{id}"),
            seq: 0,
            lane: lane.clone(),
            timestamp: *timestamp,
            run_id: run_id.clone(),
        }),
        _ => None,
    }
}
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
    pub steer_messages: Vec<AgentMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    Background,
    Subagent,
}

#[derive(Debug, Clone)]
pub struct Lane {
    pub name: String,
    pub session_id: String,
    pub parent_lane: Option<String>,
    pub leaf_id: Option<String>,
    pub status: LaneStatus,
    pub queue: LaneQueue,
    pub op_log: Vec<OpRecord>,
    pub active_run_id: Option<String>,
    session_file: Option<PathBuf>,
    pub accumulated_usage: TokenUsage,
}

impl Lane {
    pub fn new(name: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            session_id: session_id.into(),
            parent_lane: None,
            leaf_id: None,
            status: LaneStatus::Idle,
            queue: LaneQueue::default(),
            op_log: Vec::new(),
            active_run_id: None,
            session_file: None,
            accumulated_usage: TokenUsage::default(),
        }
    }
}

fn persist_queue_intent(
    lane: &mut Lane,
    queue: QueueKind,
    priority: Option<threadlane_agent::SteerPriority>,
    target: AgentMessage,
) -> Result<(), String> {
    let session_file = lane.session_file.as_deref().ok_or_else(|| {
        format!(
            "Lane '{}:{}' has no session file",
            lane.session_id, lane.name
        )
    })?;
    let seq = lane.op_log.iter().map(OpRecord::seq).max().unwrap_or(0) + 1;
    let record = OpRecord::QueueEnqueued {
        id: format!("queue-{}-{}-{seq}", lane.session_id, lane.name),
        seq,
        lane: lane.name.clone(),
        timestamp: now_ms() as u64,
        run_id: lane.active_run_id.clone(),
        queue: queue.clone(),
        priority,
        target: target.clone(),
    };
    if let Some(parent) = session_file.parent() {
        if !parent.exists() {
            return Err(format!(
                "Failed to append queue intent: parent does not exist: {}",
                parent.display()
            ));
        }
    }
    append_v2_lifecycle_record(
        session_file,
        HarnessRecord::QueueEnqueued {
            id: format!("v2-{}", record.id()),
            seq: 0,
            lane: lane.name.clone(),
            timestamp: now_ms() as u64,
            run_id: lane.active_run_id.clone(),
            queue: match queue {
                QueueKind::Steer => HarnessQueueKind::Steer,
                QueueKind::FollowUp => HarnessQueueKind::FollowUp,
                QueueKind::NextRun => HarnessQueueKind::NextRun,
            },
            priority,
            target: ProvisionedEntry {
                id: format!("queue-{}", record.id()),
                parent_id: lane.leaf_id.clone(),
                message: target.clone(),
            },
        },
    )?;
    lane.op_log.push(record);
    Ok(())
}

fn append_abort_requested_record(
    session_file: &Path,
    lane: &str,
    run_id: &str,
) -> Result<OpRecord, String> {
    let record = OpRecord::AbortRequested {
        id: format!("abort-{run_id}"),
        seq: 0,
        lane: lane.to_owned(),
        timestamp: now_ms() as u64,
        run_id: run_id.to_owned(),
    };
    let has_v2_run = JsonlStore::open(session_file)
        .ok()
        .and_then(|store| Reducer::reduce(&store).ok())
        .and_then(|state| {
            state
                .lane(lane)
                .and_then(|lane| lane.open_operation.clone())
        })
        .is_some_and(|open_run| open_run == run_id);
    if has_v2_run {
        append_v2_lifecycle_record(
            session_file,
            HarnessRecord::AbortRequested {
                id: format!("v2-{}", record.id()),
                seq: 0,
                lane: lane.to_owned(),
                timestamp: now_ms() as u64,
                run_id: run_id.to_owned(),
            },
        )?;
        return Ok(record);
    }
    append_v2_lifecycle_record(
        session_file,
        HarnessRecord::AbortRequested {
            id: format!("v2-{}", record.id()),
            seq: 0,
            lane: lane.to_owned(),
            timestamp: now_ms() as u64,
            run_id: run_id.to_owned(),
        },
    )?;
    Ok(record)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    path: PathBuf,
    name: String,
    last_selected_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub project_id: String,
    pub session_id: String,
    pub session_file: Option<PathBuf>,
    pub parent_task_id: Option<String>,
    pub kind: TaskKind,
    pub agent: String,
    pub summary: String,
    pub current_activity: Option<String>,
    pub status: TaskStatus,
    pub started_at_ms: u128,
    pub finished_at_ms: Option<u128>,
}

impl TaskRecord {
    pub fn cancellable(&self) -> bool {
        self.kind == TaskKind::Background && self.active()
    }

    pub fn active(&self) -> bool {
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
}

struct TaskRuntime {
    agent: Arc<tokio::sync::Mutex<CodingAgent>>,
    status: TaskStatus,
    prompt_lock: Arc<tokio::sync::Mutex<()>>,
    run_handle: Option<tokio::task::AbortHandle>,
    cancellation_guard: Option<SubagentCancellationGuard>,
    recovery_loaded: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ToolOutputCache {
    cache: HashMap<String, String>,
}

impl ToolOutputCache {
    pub fn get(&self, key: &str) -> Option<String> {
        self.cache.get(key).cloned()
    }

    pub fn put(&mut self, key: String, output: String) {
        self.cache.insert(key, output);
    }

    pub fn invalidate_all(&mut self) {
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
    metrics: Arc<Mutex<threadlane_agent::HarnessMetrics>>,
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
            metrics: Arc::new(Mutex::new(threadlane_agent::HarnessMetrics::default())),
            output_cache: Arc::new(Mutex::new(ToolOutputCache::default())),
            event_tx,
        };
        supervisor.load_registry();
        supervisor
    }

    pub fn output_cache(&self) -> Arc<Mutex<ToolOutputCache>> {
        self.output_cache.clone()
    }

    pub fn record_lane_usage(&self, session_id: &str, lane_name: &str, usage: &TokenUsage) {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        if let Some(lane) = lock.get_mut(&key) {
            lane.accumulated_usage.input_tokens += usage.input_tokens;
            lane.accumulated_usage.output_tokens += usage.output_tokens;
            lane.accumulated_usage.total_tokens += usage.total_tokens;
        }
    }

    pub fn aggregate_tree_usage(&self, session_id: &str, root_lane: &str) -> TokenUsage {
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

    pub fn metrics(&self) -> threadlane_agent::HarnessMetrics {
        let mut m = self.metrics.lock().unwrap().clone();
        m.active_lanes = self.lanes.lock().unwrap().len();
        m
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskAgentEvent> {
        self.event_tx.subscribe()
    }

    pub fn subscribe_lane(
        &self,
        _session_id: &str,
        _lane_name: &str,
    ) -> broadcast::Receiver<TaskAgentEvent> {
        self.event_tx.subscribe()
    }

    pub fn get_or_create_lane(&self, session_id: &str, lane_name: &str) -> Lane {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        lock.entry(key)
            .or_insert_with(|| Lane::new(lane_name, session_id))
            .clone()
    }

    pub fn get_or_create_sub_lane(
        &self,
        session_id: &str,
        lane_name: &str,
        parent_lane: &str,
    ) -> Lane {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock.entry(key).or_insert_with(|| {
            let mut l = Lane::new(lane_name, session_id);
            l.parent_lane = Some(parent_lane.to_string());
            l
        });
        lane.clone()
    }

    pub fn cancel_lane_hierarchy(&self, session_id: &str, root_lane: &str) -> usize {
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

    pub fn enqueue_steer(
        &self,
        session_id: &str,
        lane_name: &str,
        message: AgentMessage,
    ) -> Result<(), String> {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock
            .entry(key)
            .or_insert_with(|| Lane::new(lane_name, session_id));
        persist_queue_intent(
            lane,
            QueueKind::Steer,
            Some(threadlane_agent::SteerPriority::Normal),
            message.clone(),
        )?;
        lane.queue.enqueue(QueueKind::Steer, message);
        Ok(())
    }

    pub fn enqueue_steer_priority(
        &self,
        session_id: &str,
        lane_name: &str,
        message: AgentMessage,
        priority: threadlane_agent::SteerPriority,
    ) -> Result<(), String> {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock
            .entry(key)
            .or_insert_with(|| Lane::new(lane_name, session_id));
        persist_queue_intent(lane, QueueKind::Steer, Some(priority), message.clone())?;
        lane.queue.enqueue_steer_with_priority(message, priority);
        Ok(())
    }

    pub fn enqueue_followup(
        &self,
        session_id: &str,
        lane_name: &str,
        message: AgentMessage,
    ) -> Result<(), String> {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock
            .entry(key)
            .or_insert_with(|| Lane::new(lane_name, session_id));
        persist_queue_intent(lane, QueueKind::FollowUp, None, message.clone())?;
        lane.queue.enqueue(QueueKind::FollowUp, message);
        Ok(())
    }

    pub fn update_lane_leaf(&self, session_id: &str, lane_name: &str, leaf_id: Option<String>) {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        if let Some(lane) = lock.get_mut(&key) {
            lane.leaf_id = leaf_id;
        }
    }

    pub fn append_lane_op_record(&self, session_id: &str, lane_name: &str, record: OpRecord) {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        if let Some(lane) = lock.get_mut(&key) {
            lane.op_log.push(record);
        }
    }

    pub fn append_persisted_lane_record(
        &self,
        session_id: &str,
        lane_name: &str,
        session_file: &Path,
        record: OpRecord,
    ) -> Result<(), String> {
        let key = format!("{session_id}:{lane_name}");
        let operation_started = matches!(&record, OpRecord::OperationStarted { .. });
        let mut lock = self.lanes.lock().unwrap();
        lock.entry(key.clone())
            .or_insert_with(|| Lane::new(lane_name, session_id));
        if let Some(v2) = v2_lifecycle_from_op_record(&record) {
            let mirror = match &v2 {
                HarnessRecord::OperationStarted { .. } => true,
                HarnessRecord::OperationFinished { run_id, .. }
                | HarnessRecord::AbortRequested { run_id, .. } => {
                    let store =
                        JsonlStore::open(session_file).map_err(|error| error.to_string())?;
                    let has_start = store.records().iter().any(|current| {
                        matches!(current, HarnessRecord::OperationStarted { id, .. } if id == run_id)
                    });
                    match &v2 {
                        HarnessRecord::OperationFinished { .. } => {
                            has_start && !store.records().iter().any(|current| {
                                matches!(current, HarnessRecord::OperationFinished { run_id: current_run, .. } if current_run == run_id)
                            })
                        }
                        HarnessRecord::AbortRequested { .. } => {
                            has_start && !store.records().iter().any(|current| {
                                matches!(current, HarnessRecord::AbortRequested { run_id: current_run, .. } if current_run == run_id)
                            })
                        }
                        _ => has_start,
                    }
                }
                _ => true,
            };
            if mirror {
                append_v2_lifecycle_record(session_file, v2)?;
            }
        }
        let lane = lock.get_mut(&key).unwrap();
        lane.session_file = Some(session_file.to_path_buf());
        if operation_started {
            lane.active_run_id = Some(record.id().to_string());
            lane.status = LaneStatus::Running;
        }
        lane.op_log.push(record);
        Ok(())
    }

    fn persist_prompt_run(
        &self,
        session_id: &str,
        session_file: &Path,
        source_leaf_id: Option<String>,
        prompt: &str,
    ) -> Result<String, String> {
        let key = format!("{session_id}:main");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock
            .entry(key)
            .or_insert_with(|| Lane::new("main", session_id));
        let started_seq = lane.op_log.iter().map(OpRecord::seq).max().unwrap_or(0) + 1;
        let run_id = format!("run-{session_id}-{started_seq}");
        let started = OpRecord::OperationStarted {
            id: run_id.clone(),
            seq: started_seq,
            lane: "main".into(),
            timestamp: now_ms() as u64,
            source_leaf_id: source_leaf_id.clone(),
            kind: "prompt".into(),
            system_prompt_override: None,
        };
        let attempt = OpRecord::TaskAttempt {
            id: format!("attempt-{run_id}"),
            seq: started_seq + 1,
            lane: "main".into(),
            timestamp: now_ms() as u64,
            run_id: run_id.clone(),
            task: prompt.to_owned(),
            attempt: 1,
        };
        if let Some(parent) = session_file.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        if !session_file.exists() {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(session_file)
                .map_err(|error| error.to_string())?;
        }
        let store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
        let mut harness = AgentHarness::new(store);
        harness
            .accept_prompt(run_id.as_str(), AgentMessage::user(prompt, Vec::new()))
            .map_err(|error| error.to_string())?;
        harness
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        lane.session_file = Some(session_file.to_path_buf());
        lane.active_run_id = Some(run_id.clone());
        lane.status = LaneStatus::Running;
        lane.op_log.extend([started, attempt]);
        Ok(run_id)
    }

    pub fn append_tool_started_record(
        &self,
        session_id: &str,
        lane_name: &str,
        session_file: &Path,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: serde_json::Value,
    ) -> Result<(), String> {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock
            .get(&key)
            .ok_or_else(|| format!("Lane '{key}' not found"))?;
        let run_id = lane
            .active_run_id
            .clone()
            .ok_or_else(|| format!("Lane '{key}' has no active run"))?;
        let tool_index = lane
            .op_log
            .iter()
            .filter(|record| {
                matches!(record, OpRecord::ToolStarted { run_id: record_run_id, .. } if record_run_id == &run_id)
            })
            .count();
        let seq = lane.op_log.iter().map(OpRecord::seq).max().unwrap_or(0) + 1;
        let record = OpRecord::ToolStarted {
            id: format!("tool-{run_id}-{tool_index}"),
            seq,
            lane: lane_name.to_string(),
            timestamp: now_ms() as u64,
            run_id,
            assistant_entry_id: String::new(),
            tool_index,
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            effective_args,
            result_entry_id: format!("result-{tool_call_id}"),
            replay: threadlane_agent::classify_tool_replay_safety(tool_name),
        };
        let latest_assistant_entry_id = JsonlStore::open(session_file)
            .map_err(|error| error.to_string())?
            .entries()
            .iter()
            .rev()
            .find_map(|entry| {
                matches!(entry.message, AgentMessage::Assistant { .. }).then(|| entry.id.clone())
            });
        let mut v2_written = false;
        if let Some(latest_assistant_entry_id) = latest_assistant_entry_id {
            let v2_record = match &record {
                OpRecord::ToolStarted {
                    id,
                    timestamp,
                    run_id,
                    assistant_entry_id: _,
                    tool_index,
                    tool_call_id,
                    tool_name,
                    effective_args,
                    result_entry_id,
                    replay,
                    ..
                } => HarnessRecord::ToolStarted {
                    id: format!("v2-{id}"),
                    seq: 0,
                    lane: lane_name.to_owned(),
                    timestamp: *timestamp,
                    run_id: run_id.clone(),
                    assistant_entry_id: latest_assistant_entry_id,
                    tool_index: *tool_index,
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    effective_args: effective_args.clone(),
                    result_entry_id: result_entry_id.clone(),
                    replay: match replay {
                        threadlane_agent::ToolReplaySafety::Safe => {
                            threadlane_agent::harness::ToolReplaySafety::Safe
                        }
                        threadlane_agent::ToolReplaySafety::Never => {
                            threadlane_agent::harness::ToolReplaySafety::Never
                        }
                    },
                },
                _ => unreachable!("tool-start method created a different record"),
            };
            append_v2_lifecycle_record(session_file, v2_record)?;
            v2_written = true;
        }
        if !v2_written {
            return Err("tool intent has no assistant entry in the V2 session".into());
        }
        let lane = lock.get_mut(&key).unwrap();
        lane.session_file = Some(session_file.to_path_buf());
        lane.op_log.push(record);
        Ok(())
    }

    pub fn append_tool_finished_record(
        &self,
        session_id: &str,
        lane_name: &str,
        session_file: &Path,
        tool_call_id: &str,
        terminate: bool,
    ) -> Result<(), String> {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let lane = lock
            .get(&key)
            .ok_or_else(|| format!("Lane '{key}' not found"))?;
        let run_id = lane
            .active_run_id
            .clone()
            .ok_or_else(|| format!("Lane '{key}' has no active run"))?;
        let intent = lane
            .op_log
            .iter()
            .rev()
            .find_map(|record| match record {
                OpRecord::ToolStarted {
                    run_id: record_run_id,
                    tool_call_id: record_tool_call_id,
                    result_entry_id,
                    ..
                } if record_run_id == &run_id && record_tool_call_id == tool_call_id => {
                    Some(result_entry_id.clone())
                }
                _ => None,
            })
            .ok_or_else(|| format!("Tool '{tool_call_id}' has no durable intent"))?;
        let seq = lane.op_log.iter().map(OpRecord::seq).max().unwrap_or(0) + 1;
        let record = OpRecord::ToolFinished {
            id: format!("tool-finished-{run_id}-{tool_call_id}"),
            seq,
            lane: lane_name.to_owned(),
            timestamp: now_ms() as u64,
            run_id,
            tool_call_id: tool_call_id.to_owned(),
            result_entry_id: intent,
            terminate,
        };
        let (record_id, record_timestamp, record_run_id, record_result_entry_id) = match &record {
            OpRecord::ToolFinished {
                id,
                timestamp,
                run_id,
                result_entry_id,
                ..
            } => (id, timestamp, run_id, result_entry_id),
            _ => unreachable!("tool-finish method created a different record"),
        };
        let has_v2_intent = JsonlStore::open(session_file)
            .map_err(|error| error.to_string())?
            .records()
            .iter()
            .any(|current| {
                matches!(
                    current,
                    HarnessRecord::ToolStarted {
                        run_id: current_run_id,
                        tool_call_id: current_call_id,
                        ..
                    } if current_run_id == record_run_id && current_call_id == tool_call_id
                )
            });
        let has_v2_completion = JsonlStore::open(session_file)
            .map_err(|error| error.to_string())?
            .records()
            .iter()
            .any(|current| {
                matches!(
                    current,
                    HarnessRecord::ToolFinished {
                        run_id: current_run_id,
                        tool_call_id: current_call_id,
                        ..
                    } if current_run_id == record_run_id && current_call_id == tool_call_id
                )
            });
        if has_v2_intent {
            if !has_v2_completion {
                append_v2_lifecycle_record(
                    session_file,
                    HarnessRecord::ToolFinished {
                        id: format!("v2-{record_id}"),
                        seq: 0,
                        lane: lane_name.to_owned(),
                        timestamp: *record_timestamp,
                        run_id: record_run_id.clone(),
                        tool_call_id: tool_call_id.to_owned(),
                        result_entry_id: record_result_entry_id.clone(),
                        terminate,
                    },
                )?;
            }
        } else {
            return Err("tool completion has no V2 tool intent".into());
        }
        let lane = lock.get_mut(&key).unwrap();
        lane.session_file = Some(session_file.to_path_buf());
        lane.op_log.push(record);
        Ok(())
    }

    pub fn checkpoint_lane(&self, session_id: &str, lane_name: &str) -> CheckpointResult {
        let key = format!("{session_id}:{lane_name}");
        let mut lock = self.lanes.lock().unwrap();
        let mut steer_messages = Vec::new();

        if let Some(lane) = lock.get_mut(&key) {
            while let Some(msg) = lane.queue.pop_steer() {
                steer_messages.push(msg);
            }
        }

        CheckpointResult { steer_messages }
    }

    pub fn restore_session_lanes(
        &self,
        session_id: &str,
        session_file: &Path,
        session_tree: &mut threadlane_agent::SessionTree,
    ) -> Result<threadlane_agent::RecoveryResult, String> {
        let _ = session_tree;
        let store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
        let state = Reducer::reduce(&store).map_err(|error| error.to_string())?;
        let mut open_operation_ids = Vec::new();
        let mut abort_requested_operation_ids = Vec::new();
        let mut lock = self.lanes.lock().unwrap();
        for lane_state in &state.lanes {
            if lane_state.open_operation.is_none() && lane_state.queued.is_empty() {
                continue;
            }
            let key = format!("{session_id}:{}", lane_state.name);
            let lane = lock
                .entry(key)
                .or_insert_with(|| Lane::new(&lane_state.name, session_id));
            lane.session_file = Some(session_file.to_path_buf());
            lane.leaf_id = lane_state.leaf_id.clone();
            lane.active_run_id = lane_state.open_operation.clone();
            lane.status = lane_state
                .open_operation
                .as_ref()
                .map(|_| LaneStatus::Suspended)
                .unwrap_or(LaneStatus::Idle);
            lane.queue = LaneQueue::default();
            for queued in &lane_state.queued {
                match queued.queue {
                    HarnessQueueKind::Steer => lane.queue.enqueue_steer_with_priority(
                        queued.target.message.clone(),
                        queued
                            .priority
                            .unwrap_or(threadlane_agent::SteerPriority::Normal),
                    ),
                    HarnessQueueKind::FollowUp => lane
                        .queue
                        .enqueue(QueueKind::FollowUp, queued.target.message.clone()),
                    HarnessQueueKind::NextRun => lane
                        .queue
                        .enqueue(QueueKind::NextRun, queued.target.message.clone()),
                }
            }
            if let Some(run_id) = lane_state.open_operation.clone() {
                open_operation_ids.push(run_id.clone());
                if lane_state.abort_requested {
                    abort_requested_operation_ids.push(run_id);
                }
            }
        }
        Ok(threadlane_agent::RecoveryResult {
            recovered_open_operations: open_operation_ids.len(),
            open_operation_ids,
            abort_requested_operation_ids,
            ..Default::default()
        })
    }

    pub fn finish_recovered_operations(
        &self,
        session_id: &str,
        session_file: &Path,
        run_ids: &[String],
        outcome: threadlane_agent::OpOutcome,
    ) -> Result<(), String> {
        for (index, run_id) in run_ids.iter().enumerate() {
            let seq = self
                .get_or_create_lane(session_id, "main")
                .op_log
                .iter()
                .map(OpRecord::seq)
                .max()
                .unwrap_or(0)
                + 1;
            self.append_persisted_lane_record(
                session_id,
                "main",
                session_file,
                OpRecord::OperationFinished {
                    id: format!("finish-recovery-{run_id}-{index}"),
                    seq,
                    lane: "main".into(),
                    timestamp: now_ms() as u64,
                    run_id: run_id.clone(),
                    outcome: outcome.clone(),
                    error: None,
                },
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

    pub fn navigate_lane(
        &self,
        session_id: &str,
        lane_name: &str,
        target_node_id: &str,
        session_tree: &mut threadlane_agent::SessionTree,
    ) -> Result<bool, String> {
        if !session_tree.nodes.contains_key(target_node_id) {
            return Err(format!("Node '{target_node_id}' not found in session tree"));
        }

        let v2_file = self
            .lanes
            .lock()
            .unwrap()
            .get(&format!("{session_id}:{lane_name}"))
            .and_then(|lane| lane.session_file.clone());
        if let Some(session_file) = v2_file {
            if let Ok(store) = JsonlStore::open(&session_file) {
                if store
                    .entries()
                    .iter()
                    .any(|entry| entry.id == target_node_id)
                {
                    let run_id = format!("navigation-{}", now_ms());
                    let mut harness = AgentHarness::new(store);
                    harness
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

        let nav_record = OpRecord::Navigation {
            id: format!("nav-{}", now_ms()),
            seq: lane.op_log.iter().map(OpRecord::seq).max().unwrap_or(0) + 1,
            lane: lane_name.to_string(),
            timestamp: now_ms() as u64,
            run_id: lane.active_run_id.clone().unwrap_or_default(),
            target_id: target_node_id.to_string(),
            summary_entry_id: None,
        };
        lane.op_log.push(nav_record);

        Ok(true)
    }

    pub fn redeem_deferred(
        &self,
        session_id: &str,
        lane_name: &str,
        result_message: AgentMessage,
        session_tree: &mut threadlane_agent::SessionTree,
    ) -> Result<String, String> {
        let key = format!("{session_id}:{lane_name}");
        let leaf_id = {
            let mut lock = self.lanes.lock().unwrap();
            let lane = lock
                .get_mut(&key)
                .ok_or_else(|| format!("Lane '{key}' not found"))?;
            lane.status = LaneStatus::Running;
            lane.leaf_id.clone()
        };

        let new_node_id = session_tree.add_message_at_leaf(leaf_id.as_deref(), result_message);

        let mut lock = self.lanes.lock().unwrap();
        if let Some(lane) = lock.get_mut(&key) {
            lane.leaf_id = Some(new_node_id.clone());
            lane.status = LaneStatus::Idle;
        }

        Ok(new_node_id)
    }

    fn registry_file(&self) -> PathBuf {
        self.global_dir.join("projects.json")
    }

    fn load_registry(&self) {
        let file = self.registry_file();
        if file.exists() {
            if let Ok(contents) = fs::read_to_string(&file) {
                if let Ok(records) = serde_json::from_str::<Vec<ProjectRecord>>(&contents) {
                    let mut lock = self.projects.lock().unwrap();
                    for rec in records {
                        lock.insert(rec.id.clone(), rec);
                    }
                }
            }
        }
    }

    fn save_registry(&self) {
        let records: Vec<ProjectRecord> = self.projects.lock().unwrap().values().cloned().collect();
        let file = self.registry_file();
        if let Ok(json) = serde_json::to_string_pretty(&records) {
            let tmp = file.with_extension("json.tmp");
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(tmp, file);
            }
        }
    }

    pub fn register_project(&self, raw_path: &Path) -> Result<ProjectRecord, String> {
        let canonical = raw_path.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize project path '{}': {e}",
                raw_path.display()
            )
        })?;

        let id = md5_hash(&canonical.to_string_lossy());
        let name = canonical
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".into());

        let record = ProjectRecord {
            id: id.clone(),
            path: canonical,
            name,
            last_selected_task_id: None,
        };

        {
            let mut lock = self.projects.lock().unwrap();
            lock.insert(id.clone(), record.clone());
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

        let coding_agent = CodingAgent::new(opts);
        let rx = coding_agent.subscribe();

        let agent_arc = Arc::new(tokio::sync::Mutex::new(coding_agent));
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
            agent: agent_arc,
            status: TaskStatus::Idle,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            run_handle: None,
            cancellation_guard: None,
            recovery_loaded: false,
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
        let tasks = self.tasks.clone();
        let runtimes = self.runtimes.clone();
        let lanes = self.lanes.clone();
        let tid = task_id.clone();
        let pid = project_id.to_string();
        let session_id = final_session_file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| tid.clone());
        let event_session_file = final_session_file.clone();
        tokio::spawn(async move {
            let mut sub_rx = rx;
            while let Ok(evt) = sub_rx.recv().await {
                observe_subagent_lane(&lanes, &session_id, &evt);
                let status = match &evt {
                    AgentEvent::AgentStart => Some(TaskStatus::Running),
                    AgentEvent::AgentEnd { .. } => Some(TaskStatus::Completed),
                    AgentEvent::AgentError { .. } => Some(TaskStatus::Failed),
                    _ => None,
                };
                {
                    let mut task_records = tasks.lock().unwrap();
                    if let Some(task) = task_records.get_mut(&tid) {
                        apply_background_event(task, &evt);
                    }
                    apply_subagent_event(
                        &mut task_records,
                        &pid,
                        &session_id,
                        Some(&event_session_file),
                        Some(&tid),
                        &evt,
                    );
                }
                if let Some(status) = status {
                    if let Some(runtime) = runtimes.lock().unwrap().get_mut(&tid) {
                        runtime.status = status;
                    }
                }
                let _ = event_tx.send(TaskAgentEvent {
                    task_id: tid.clone(),
                    project_id: pid.clone(),
                    lane: Some("main".into()),
                    event: evt,
                });
            }
        });

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
            .map(|(task_id, runtime)| {
                (
                    task_id.clone(),
                    runtime.prompt_lock.clone(),
                    runtime.agent.clone(),
                )
            })
            .collect();

        let mut reloaded = 0;
        let mut failures = Vec::new();
        for (task_id, prompt_lock, agent) in targets {
            let _prompt_guard = prompt_lock.lock().await;
            let mut agent = agent.lock().await;
            match agent.reload_extensions().await {
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

    pub fn submit_input(&self, task_id: &str, prompt: String) -> Result<(), String> {
        let (agent_arc, prompt_lock, session_id, session_file) = {
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
            (
                rt.agent.clone(),
                rt.prompt_lock.clone(),
                task.session_id,
                task.session_file,
            )
        };

        let lane = self.get_or_create_lane(&session_id, "main");
        let session_file_for_log = session_file
            .as_deref()
            .ok_or_else(|| format!("Task ID '{task_id}' has no session file"))?;
        let run_id = self.persist_prompt_run(
            &session_id,
            session_file_for_log,
            lane.leaf_id.clone(),
            &prompt,
        )?;

        if let Some(task) = self.tasks.lock().unwrap().get_mut(task_id) {
            task.summary = prompt.clone();
        }
        self.update_task_status(task_id, TaskStatus::Running);

        let tid = task_id.to_string();
        let tasks_map = self.tasks.clone();
        let runtimes_map = self.runtimes.clone();
        let supervisor = self.clone();
        let session_file_for_run = session_file.clone();
        let session_id_for_run = session_id.to_string();
        let run_id_for_run = run_id.clone();

        let handle = tokio::spawn(async move {
            let _guard = prompt_lock.lock().await;
            let cancellation_guard = runtimes_map
                .lock()
                .unwrap()
                .get_mut(&tid)
                .and_then(|runtime| runtime.cancellation_guard.take());
            drop(cancellation_guard);
            let mut agent = agent_arc.lock().await;
            let should_restore = {
                let mut runtimes = runtimes_map.lock().unwrap();
                runtimes
                    .get_mut(&tid)
                    .map(|runtime| {
                        let restore = !runtime.recovery_loaded;
                        restore
                    })
                    .unwrap_or(false)
            };
            if should_restore {
                if let Some(session_file) = session_file_for_run.as_deref() {
                    match supervisor.restore_session_lanes(
                        &session_id_for_run,
                        session_file,
                        &mut agent.session_tree,
                    ) {
                        Ok(recovery) => {
                            if let Ok(mut runtimes) = runtimes_map.lock() {
                                if let Some(runtime) = runtimes.get_mut(&tid) {
                                    runtime.recovery_loaded = true;
                                }
                            }
                            let replayed = agent
                                .replay_safe_tools(&recovery.safe_tools_to_replay)
                                .await;
                            let replay_failed = replayed.iter().any(|result| result.is_error);
                            for (record, result) in
                                recovery.safe_tools_to_replay.iter().zip(replayed.iter())
                            {
                                if let threadlane_agent::OpRecord::ToolStarted {
                                    tool_call_id,
                                    ..
                                } = record
                                {
                                    agent.session_tree.replace_tool_result(
                                        tool_call_id,
                                        result.content.clone(),
                                        result.is_error,
                                    );
                                }
                            }
                            let recovered_run_ids: Vec<String> = recovery
                                .open_operation_ids
                                .iter()
                                .filter(|run_id| *run_id != &run_id_for_run)
                                .cloned()
                                .collect();
                            if !recovered_run_ids.is_empty() {
                                let recovery_outcome = if recovery.unreplayable_tools > 0 {
                                    threadlane_agent::OpOutcome::Aborted
                                } else if recovered_run_ids.iter().any(|run_id| {
                                    recovery.abort_requested_operation_ids.contains(run_id)
                                }) {
                                    threadlane_agent::OpOutcome::Aborted
                                } else if replay_failed {
                                    threadlane_agent::OpOutcome::Failed
                                } else {
                                    threadlane_agent::OpOutcome::Completed
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
                        Err(_error) => {
                            let _ = supervisor.update_task_status(&tid, TaskStatus::Failed);
                            return;
                        }
                    }
                }
            }
            if let Some(session_file) = session_file_for_run.as_deref() {
                let recorder_supervisor = supervisor.clone();
                let recorder_session_id = session_id_for_run.clone();
                let recorder_session_file = session_file.to_path_buf();
                agent.set_tool_intent_recorder(Some(Arc::new(move |id, name, arguments| {
                    let id = id.to_string();
                    let name = name.to_string();
                    let arguments = arguments.to_string();
                    let recorder_supervisor = recorder_supervisor.clone();
                    let recorder_session_id = recorder_session_id.clone();
                    let recorder_session_file = recorder_session_file.clone();
                    Box::pin(async move {
                        if child_task_id(&id).is_some() {
                            return Ok(());
                        }
                        let effective_args = serde_json::from_str(&arguments)
                            .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()));
                        recorder_supervisor.append_tool_started_record(
                            &recorder_session_id,
                            "main",
                            &recorder_session_file,
                            &id,
                            &name,
                            effective_args,
                        )
                    })
                })));
                let completion_supervisor = supervisor.clone();
                let completion_session_id = session_id_for_run.clone();
                let completion_session_file = session_file.to_path_buf();
                agent.set_tool_completion_recorder(Some(Arc::new(
                    move |tool_call_id, terminate| {
                        let completion_supervisor = completion_supervisor.clone();
                        let completion_session_id = completion_session_id.clone();
                        let completion_session_file = completion_session_file.clone();
                        let tool_call_id = tool_call_id.to_owned();
                        Box::pin(async move {
                            completion_supervisor.append_tool_finished_record(
                                &completion_session_id,
                                "main",
                                &completion_session_file,
                                &tool_call_id,
                                terminate,
                            )
                        })
                    },
                )));
            }
            if let Err(error) = agent.adopt_harness_run(&run_id_for_run) {
                let _ = supervisor.update_task_status(&tid, TaskStatus::Failed);
                eprintln!("failed to adopt supervisor harness run {run_id_for_run}: {error}");
                return;
            }
            let input_result = agent.handle_input(&prompt).await;
            agent.set_tool_intent_recorder(None);
            agent.set_tool_completion_recorder(None);
            let (outcome, error) = match input_result {
                Some(Err(error)) => (threadlane_agent::OpOutcome::Failed, Some(error)),
                _ => (threadlane_agent::OpOutcome::Completed, None),
            };
            let task_status = if error.is_some() {
                TaskStatus::Failed
            } else {
                TaskStatus::Completed
            };
            let run_is_active = supervisor
                .lanes
                .lock()
                .unwrap()
                .get(&format!("{session_id_for_run}:main"))
                .and_then(|lane| lane.active_run_id.as_deref())
                == Some(run_id_for_run.as_str());

            if run_is_active {
                if let Some(session_file) = session_file_for_run.as_deref() {
                    let seq = supervisor
                        .get_or_create_lane(&session_id_for_run, "main")
                        .op_log
                        .len() as u64
                        + 1;
                    let _ = supervisor.append_persisted_lane_record(
                        &session_id_for_run,
                        "main",
                        session_file,
                        OpRecord::OperationFinished {
                            id: format!("finish-{}", now_ms()),
                            seq,
                            lane: "main".into(),
                            timestamp: now_ms() as u64,
                            run_id: run_id_for_run.clone(),
                            outcome,
                            error,
                        },
                    );
                }
            }
            if run_is_active {
                let mut lanes = supervisor.lanes.lock().unwrap();
                if let Some(lane) = lanes.get_mut(&format!("{session_id_for_run}:main")) {
                    lane.status = LaneStatus::Idle;
                    lane.active_run_id = None;
                }
            }

            let mut t_lock = tasks_map.lock().unwrap();
            if let Some(tr) = t_lock.get_mut(&tid) {
                if tr.status == TaskStatus::Running {
                    tr.status = task_status;
                    tr.current_activity = None;
                    tr.finished_at_ms = Some(now_ms());
                }
            }
            let mut r_lock = runtimes_map.lock().unwrap();
            if let Some(rt) = r_lock.get_mut(&tid) {
                if rt.status == TaskStatus::Running {
                    rt.status = task_status;
                }
                rt.run_handle = None;
            }
        });
        if let Some(runtime) = self.runtimes.lock().unwrap().get_mut(task_id) {
            runtime.run_handle = Some(handle.abort_handle());
        }

        Ok(())
    }

    pub fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        let active_run = {
            let task = self.tasks.lock().unwrap().get(task_id).cloned();
            task.filter(|task| task.active()).and_then(|task| {
                let run_id = self
                    .lanes
                    .lock()
                    .unwrap()
                    .get(&format!("{}:main", task.session_id))
                    .and_then(|lane| lane.active_run_id.clone());
                task.session_file
                    .map(|session_file| (task.session_id, session_file, run_id))
            })
        };
        let cancellation_guard = if let Some((session_id, session_file, run_id)) = active_run {
            if let Some(run_id) = run_id.as_deref() {
                let abort_record = append_abort_requested_record(&session_file, "main", run_id)?;
                if let Some(lane) = self
                    .lanes
                    .lock()
                    .unwrap()
                    .get_mut(&format!("{session_id}:main"))
                {
                    lane.op_log.push(abort_record);
                }
            }
            let guard = abort_open_subagent_operations(&session_file)?;
            if let Some(run_id) = run_id {
                let _ = self.finish_recovered_operations(
                    &session_id,
                    &session_file,
                    std::slice::from_ref(&run_id),
                    threadlane_agent::OpOutcome::Aborted,
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
                drop(cancellation_guard);
                None
            }
        };
        if let Some(handle) = handle {
            handle.abort();
        }
        let finished_at_ms = now_ms();
        let mut tasks = self.tasks.lock().unwrap();
        let Some(task) = tasks.get_mut(task_id) else {
            return Err(format!("Task ID '{task_id}' not found"));
        };
        task.status = TaskStatus::Cancelled;
        task.current_activity = None;
        task.finished_at_ms = Some(finished_at_ms);
        for child in tasks.values_mut() {
            if child.parent_task_id.as_deref() == Some(task_id) && child.active() {
                child.status = TaskStatus::Cancelled;
                child.current_activity = None;
                child.finished_at_ms = Some(finished_at_ms);
            }
        }
        drop(tasks);
        if let Some(runtime) = self.runtimes.lock().unwrap().get_mut(task_id) {
            runtime.status = TaskStatus::Cancelled;
        }
        Ok(())
    }

    pub fn resume_task(&self, task_id: &str) -> Result<(), String> {
        let summary = {
            let task = self
                .tasks
                .lock()
                .unwrap()
                .get(task_id)
                .cloned()
                .ok_or_else(|| format!("Task ID '{task_id}' not found"))?;
            if task.status == TaskStatus::Running || task.status == TaskStatus::Idle {
                return Err("Task is already running".into());
            }
            task.summary.clone()
        };
        if summary.is_empty() {
            return Err("Task has no prompt to resume".into());
        }
        self.update_task_status(task_id, TaskStatus::Running);
        self.submit_input(task_id, summary)
    }

    fn update_task_status(&self, task_id: &str, status: TaskStatus) {
        let mut t_lock = self.tasks.lock().unwrap();
        if let Some(tr) = t_lock.get_mut(task_id) {
            tr.status = status;
            if matches!(
                status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Interrupted
            ) {
                tr.finished_at_ms = Some(now_ms());
            }
        }
        let mut r_lock = self.runtimes.lock().unwrap();
        if let Some(rt) = r_lock.get_mut(task_id) {
            rt.status = status;
        }
    }

    pub fn get_task_status(&self, task_id: &str) -> Option<TaskStatus> {
        let lock = self.tasks.lock().unwrap();
        lock.get(task_id).map(|t| t.status)
    }

    pub fn get_task(&self, task_id: &str) -> Option<TaskRecord> {
        let lock = self.tasks.lock().unwrap();
        lock.get(task_id).cloned()
    }

    pub fn list_tasks_for_project(&self, project_id: &str) -> Vec<TaskRecord> {
        let lock = self.tasks.lock().unwrap();
        let mut tasks = lock
            .values()
            .filter(|t| t.project_id == project_id)
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| {
            right
                .started_at_ms
                .cmp(&left.started_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        tasks
    }

    pub fn observe_session_event(
        &self,
        project_id: &str,
        session_id: &str,
        session_file: Option<&Path>,
        event: &AgentEvent,
    ) -> bool {
        observe_subagent_lane(&self.lanes, session_id, event);
        apply_subagent_event(
            &mut self.tasks.lock().unwrap(),
            project_id,
            session_id,
            session_file,
            None,
            event,
        )
    }

    pub fn finish_session_tasks(&self, project_id: &str, session_id: &str) -> bool {
        let mut changed = false;
        let finished_at_ms = now_ms();
        for task in self.tasks.lock().unwrap().values_mut() {
            if task.project_id == project_id
                && task.session_id == session_id
                && task.kind == TaskKind::Subagent
                && task.active()
            {
                task.status = TaskStatus::Cancelled;
                task.current_activity = None;
                task.finished_at_ms = Some(finished_at_ms);
                changed = true;
            }
        }
        changed
    }
}

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

fn apply_background_event(task: &mut TaskRecord, event: &AgentEvent) -> bool {
    match event {
        AgentEvent::AgentStart => {
            task.status = TaskStatus::Running;
            true
        }
        AgentEvent::AgentEnd { .. } => {
            task.status = TaskStatus::Completed;
            task.current_activity = None;
            task.finished_at_ms = Some(now_ms());
            true
        }
        AgentEvent::AgentError { error } => {
            task.status = TaskStatus::Failed;
            task.current_activity = Some(error.clone());
            task.finished_at_ms = Some(now_ms());
            true
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id, name, ..
        } if child_task_id(tool_call_id).is_none() => {
            task.current_activity = Some(name.clone());
            true
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, .. }
            if child_task_id(tool_call_id).is_none() =>
        {
            task.current_activity = None;
            true
        }
        _ => false,
    }
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

fn apply_subagent_event(
    tasks: &mut HashMap<String, TaskRecord>,
    project_id: &str,
    session_id: &str,
    session_file: Option<&Path>,
    parent_task_id: Option<&str>,
    event: &AgentEvent,
) -> bool {
    match event {
        AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent,
            task,
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            tasks.insert(
                id.clone(),
                TaskRecord {
                    id,
                    project_id: project_id.to_owned(),
                    session_id: session_id.to_owned(),
                    session_file: session_file.map(Path::to_path_buf),
                    parent_task_id: parent_task_id.map(str::to_owned),
                    kind: TaskKind::Subagent,
                    agent: agent.clone(),
                    summary: task.clone(),
                    current_activity: None,
                    status: TaskStatus::Idle,
                    started_at_ms: now_ms(),
                    finished_at_ms: None,
                },
            );
            true
        }
        AgentEvent::SubagentStarted {
            run_id, task_index, ..
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.status = TaskStatus::Running;
            true
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id, name, ..
        } => {
            let Some(id) = child_task_id(tool_call_id) else {
                return false;
            };
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.current_activity = Some(name.clone());
            true
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
            let Some(id) = child_task_id(tool_call_id) else {
                return false;
            };
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.current_activity = None;
            true
        }
        AgentEvent::SubagentFinished {
            run_id,
            task_index,
            succeeded,
            error,
            ..
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.status = if *succeeded {
                TaskStatus::Completed
            } else {
                TaskStatus::Failed
            };
            task.current_activity = error.clone();
            task.finished_at_ms = Some(now_ms());
            true
        }
        _ => false,
    }
}

fn md5_hash(input: &str) -> String {
    format!("{:x}", md5::compute(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use threadlane_agent::TokenUsage;

    #[test]
    fn abort_request_is_persisted_before_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        append_v2_lifecycle_record(
            &session_file,
            HarnessRecord::OperationStarted {
                id: "run-1".into(),
                seq: 0,
                lane: "main".into(),
                timestamp: 1,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            },
        )
        .unwrap();
        let record = append_abort_requested_record(&session_file, "main", "run-1").unwrap();
        assert!(matches!(record, OpRecord::AbortRequested { ref run_id, .. } if run_id == "run-1"));
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
        let mut harness = AgentHarness::new(JsonlStore::open(&session_file).unwrap());
        harness
            .accept_prompt("run-v2", AgentMessage::user("prompt", Vec::new()))
            .unwrap();
        harness.drive_to_completion().unwrap();

        append_abort_requested_record(&session_file, "main", "run-v2").unwrap();

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
        let first = supervisor
            .persist_prompt_run("session-1", &session_file, None, "one")
            .unwrap();
        supervisor
            .append_persisted_lane_record(
                "session-1",
                "main",
                &session_file,
                OpRecord::OperationFinished {
                    id: "finish-first".into(),
                    seq: 3,
                    lane: "main".into(),
                    timestamp: 3,
                    run_id: first.clone(),
                    outcome: threadlane_agent::OpOutcome::Completed,
                    error: None,
                },
            )
            .unwrap();
        let second = supervisor
            .persist_prompt_run("session-1", &session_file, None, "two")
            .unwrap();
        assert_ne!(first, second);
        let harness = JsonlStore::open(&session_file).unwrap();
        assert_eq!(
            harness
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
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        supervisor
            .append_persisted_lane_record(
                "session-1",
                "main",
                &session_file,
                OpRecord::OperationStarted {
                    id: "run-1".into(),
                    seq: 1,
                    lane: "main".into(),
                    timestamp: 1,
                    source_leaf_id: None,
                    kind: "prompt".into(),
                    system_prompt_override: None,
                },
            )
            .unwrap();
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: "test_key".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(session_file.clone()),
            system_prompt: Default::default(),
        });
        let recorder_supervisor = supervisor.clone();
        let recorder_session_file = session_file.clone();
        agent.set_tool_intent_recorder(Some(Arc::new(move |id, name, arguments| {
            let id = id.to_string();
            let name = name.to_string();
            let arguments = arguments.to_string();
            let recorder_supervisor = recorder_supervisor.clone();
            let recorder_session_file = recorder_session_file.clone();
            Box::pin(async move {
                recorder_supervisor.append_tool_started_record(
                    "session-1",
                    "main",
                    &recorder_session_file,
                    &id,
                    &name,
                    serde_json::from_str(&arguments).unwrap(),
                )
            })
        })));

        let results = agent
            .replay_safe_tools(&[OpRecord::ToolStarted {
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
                replay: threadlane_agent::ToolReplaySafety::Safe,
            }])
            .await;

        assert!(!results[0].is_error);
        let harness = JsonlStore::open(&session_file).unwrap();
        assert!(harness.records().iter().any(|record| {
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

        let harness = JsonlStore::open(&session_file).unwrap();
        let finished = harness
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
        supervisor
            .append_persisted_lane_record(
                "session-1",
                "main",
                &session_file,
                OpRecord::OperationStarted {
                    id: "run-1".into(),
                    seq: 1,
                    lane: "main".into(),
                    timestamp: 1,
                    source_leaf_id: None,
                    kind: "prompt".into(),
                    system_prompt_override: None,
                },
            )
            .unwrap();
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
        let mut tree = threadlane_agent::SessionTree::new("session-1");
        let recovery = supervisor
            .restore_session_lanes("session-1", &session_file, &mut tree)
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
        for (index, (lane, run_id)) in [
            ("subagent-1:0", "run-open-1"),
            ("subagent-1:1", "run-open-2"),
            ("subagent-1:2", "run-finished"),
        ]
        .into_iter()
        .enumerate()
        {
            supervisor
                .append_persisted_lane_record(
                    "session-1",
                    lane,
                    &session_file,
                    OpRecord::OperationStarted {
                        id: run_id.into(),
                        seq: index as u64 + 1,
                        lane: lane.into(),
                        timestamp: 1,
                        source_leaf_id: None,
                        kind: "subagent".into(),
                        system_prompt_override: None,
                    },
                )
                .unwrap();
        }
        supervisor
            .append_persisted_lane_record(
                "session-1",
                "subagent-1:2",
                &session_file,
                OpRecord::OperationFinished {
                    id: "finish-run-finished".into(),
                    seq: 4,
                    lane: "subagent-1:2".into(),
                    timestamp: 1,
                    run_id: "run-finished".into(),
                    outcome: threadlane_agent::OpOutcome::Completed,
                    error: None,
                },
            )
            .unwrap();

        supervisor.cancel_task("task-1").unwrap();

        let harness = JsonlStore::open(&session_file).unwrap();
        let aborted: Vec<_> = harness
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
        for run_id in ["run-open-1", "run-open-2", "run-finished"] {
            assert_eq!(
                harness
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
    fn subagent_lifecycle_is_tracked_under_its_session() {
        let mut tasks = HashMap::new();
        let session_file = PathBuf::from("/repo/.threadlane/sessions/chat.jsonl");
        assert!(apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            Some(&session_file),
            None,
            &AgentEvent::SubagentQueued {
                run_id: 7,
                task_index: 1,
                agent: "reviewer".into(),
                task: "Review the patch".into(),
            },
        ));
        let task = &tasks["subagent-7:1"];
        assert_eq!(task.kind, TaskKind::Subagent);
        assert_eq!(task.project_id, "project-1");
        assert_eq!(task.session_id, "chat");
        assert_eq!(task.agent, "reviewer");
        assert_eq!(task.summary, "Review the patch");
        assert_eq!(task.status, TaskStatus::Idle);
        assert!(!task.cancellable());

        assert!(apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            Some(&session_file),
            None,
            &AgentEvent::SubagentStarted {
                run_id: 7,
                task_index: 1,
                journal_run_id: "subagent-run-1".into(),
            },
        ));
        assert_eq!(tasks["subagent-7:1"].status, TaskStatus::Running);
    }

    #[test]
    fn child_tool_events_update_only_the_matching_subagent() {
        let mut tasks = HashMap::new();
        let session_file = PathBuf::from("/repo/.threadlane/sessions/chat.jsonl");
        for task_index in 0..2 {
            apply_subagent_event(
                &mut tasks,
                "project-1",
                "chat",
                Some(&session_file),
                None,
                &AgentEvent::SubagentQueued {
                    run_id: 8,
                    task_index,
                    agent: format!("worker-{task_index}"),
                    task: format!("Task {task_index}"),
                },
            );
        }
        apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            Some(&session_file),
            None,
            &AgentEvent::ToolExecutionStart {
                tool_call_id: "subagent-8:1:read-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"src/lib.rs"}"#.into(),
            },
        );
        assert_eq!(tasks["subagent-8:0"].current_activity, None);
        assert_eq!(
            tasks["subagent-8:1"].current_activity.as_deref(),
            Some("read_file")
        );
    }

    #[test]
    fn subagent_finish_records_failure() {
        let mut tasks = HashMap::new();
        apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            None,
            Some("task-parent"),
            &AgentEvent::SubagentQueued {
                run_id: 9,
                task_index: 0,
                agent: "worker".into(),
                task: "Implement".into(),
            },
        );
        apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            None,
            Some("task-parent"),
            &AgentEvent::SubagentFinished {
                run_id: 9,
                task_index: 0,
                journal_run_id: "subagent-run-2".into(),
                succeeded: false,
                error: Some("provider failed".into()),
            },
        );
        let task = &tasks["subagent-9:0"];
        assert_eq!(task.parent_task_id.as_deref(), Some("task-parent"));
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.current_activity.as_deref(), Some("provider failed"));
        assert!(task.finished_at_ms.is_some());
    }

    #[test]
    fn background_task_tracks_current_tool_and_completion() {
        let mut task = TaskRecord {
            id: "task-1".into(),
            project_id: "project-1".into(),
            session_id: "task-1".into(),
            session_file: None,
            parent_task_id: None,
            kind: TaskKind::Background,
            agent: "task".into(),
            summary: "Run checks".into(),
            current_activity: None,
            status: TaskStatus::Running,
            started_at_ms: 1,
            finished_at_ms: None,
        };
        assert!(apply_background_event(
            &mut task,
            &AgentEvent::ToolExecutionStart {
                tool_call_id: "command-1".into(),
                name: "run_command".into(),
                arguments: r#"{"command":"cargo check"}"#.into(),
            },
        ));
        assert_eq!(task.current_activity.as_deref(), Some("run_command"));
        assert!(apply_background_event(
            &mut task,
            &AgentEvent::AgentEnd {
                usage: TokenUsage::default(),
            },
        ));
        assert_eq!(task.status, TaskStatus::Completed);
        assert!(task.finished_at_ms.is_some());
    }

    #[test]
    fn unfinished_session_subagents_are_cancelled_when_parent_run_ends() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        supervisor.observe_session_event(
            "project-1",
            "chat",
            None,
            &AgentEvent::SubagentQueued {
                run_id: 10,
                task_index: 0,
                agent: "worker".into(),
                task: "Inspect".into(),
            },
        );
        assert!(supervisor.finish_session_tasks("project-1", "chat"));
        let task = supervisor
            .list_tasks_for_project("project-1")
            .into_iter()
            .find(|task| task.id == "subagent-10:0")
            .unwrap();
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert!(task.finished_at_ms.is_some());
    }

    #[test]
    fn supervisor_lane_management_and_queuing() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");

        let lane = supervisor.get_or_create_lane("session-1", "main");
        assert_eq!(lane.name, "main");
        assert_eq!(lane.status, LaneStatus::Idle);

        supervisor
            .append_persisted_lane_record(
                "session-1",
                "main",
                &session_file,
                OpRecord::OperationStarted {
                    id: "run-1".into(),
                    seq: 1,
                    lane: "main".into(),
                    timestamp: 1,
                    source_leaf_id: None,
                    kind: "prompt".into(),
                    system_prompt_override: None,
                },
            )
            .unwrap();
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
        assert_eq!(updated_lane.queue.steer.len(), 1);
        let harness = JsonlStore::open(&session_file).unwrap();
        assert!(harness.records().iter().any(|record| matches!(
            record,
            HarnessRecord::QueueEnqueued {
                queue: HarnessQueueKind::Steer,
                priority: Some(threadlane_agent::SteerPriority::Normal),
                target,
                ..
            } if matches!(target.message, AgentMessage::User { ref content } if content == "steer msg")
        )));
    }

    #[test]
    fn queue_enqueued_is_persisted_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        supervisor
            .append_persisted_lane_record(
                "session-1",
                "main",
                &session_file,
                OpRecord::OperationStarted {
                    id: "run-1".into(),
                    seq: 1,
                    lane: "main".into(),
                    timestamp: 1,
                    source_leaf_id: None,
                    kind: "prompt".into(),
                    system_prompt_override: None,
                },
            )
            .unwrap();

        supervisor
            .enqueue_followup(
                "session-1",
                "main",
                AgentMessage::User {
                    content: "next".into(),
                },
            )
            .unwrap();
        supervisor
            .enqueue_steer_priority(
                "session-1",
                "main",
                AgentMessage::User {
                    content: "urgent".into(),
                },
                threadlane_agent::SteerPriority::High,
            )
            .unwrap();
        let harness = JsonlStore::open(&session_file).unwrap();
        assert!(matches!(
            harness.records().iter().find(|record| matches!(record, HarnessRecord::QueueEnqueued { target, .. } if matches!(target.message, AgentMessage::User { ref content } if content == "next"))),
            Some(HarnessRecord::QueueEnqueued {
                queue: HarnessQueueKind::FollowUp,
                target: ProvisionedEntry { message: AgentMessage::User { content }, .. },
                ..
            }) if content == "next"
        ));
        assert!(matches!(
            harness.records().iter().find(|record| matches!(record, HarnessRecord::QueueEnqueued { target, .. } if matches!(target.message, AgentMessage::User { ref content } if content == "urgent"))),
            Some(HarnessRecord::QueueEnqueued {
                queue: HarnessQueueKind::Steer,
                priority: Some(threadlane_agent::SteerPriority::High),
                target: ProvisionedEntry { message: AgentMessage::User { content }, .. },
                ..
            }) if content == "urgent"
        ));

        let mut tree = threadlane_agent::SessionTree::new("session-1");
        supervisor
            .restore_session_lanes("session-1", &session_file, &mut tree)
            .unwrap();
        supervisor
            .restore_session_lanes("session-1", &session_file, &mut tree)
            .unwrap();
        let restored = supervisor.get_or_create_lane("session-1", "main");
        assert_eq!(restored.queue.follow_up.len(), 1);
        assert_eq!(restored.queue.steer.len(), 1);
        assert_eq!(
            restored.queue.steer[0].priority,
            threadlane_agent::SteerPriority::High
        );

        supervisor
            .lanes
            .lock()
            .unwrap()
            .get_mut("session-1:main")
            .unwrap()
            .session_file = Some(dir.path().join("missing/session.jsonl"));
        assert!(supervisor
            .enqueue_followup(
                "session-1",
                "main",
                AgentMessage::User {
                    content: "must not queue".into(),
                },
            )
            .is_err());
        assert_eq!(
            supervisor
                .get_or_create_lane("session-1", "main")
                .queue
                .follow_up
                .len(),
            1
        );
    }

    #[test]
    fn v2_tool_intent_does_not_create_a_legacy_sidecar_record() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        let mut harness = AgentHarness::new(JsonlStore::open(&session_file).unwrap());
        harness
            .accept_prompt("run-v2", AgentMessage::user("prompt", Vec::new()))
            .unwrap();
        harness.drive_to_completion().unwrap();
        let store = harness.store();
        let prompt_entry = store
            .entries()
            .iter()
            .find(|entry| entry.id == "entry-run-v2-user")
            .unwrap();
        let assistant_seq = store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(store.records().iter().map(HarnessRecord::seq))
            .max()
            .unwrap_or(prompt_entry.seq)
            + 1;
        let mut harness = AgentHarness::new(JsonlStore::open(&session_file).unwrap());
        harness
            .append_entry_gated(threadlane_agent::harness::Entry {
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
                terminate: false,
            })
            .unwrap();
        harness.drive_to_completion().unwrap();
        {
            let mut lanes = supervisor.lanes.lock().unwrap();
            let lane = lanes
                .entry("session-1:main".into())
                .or_insert_with(|| Lane::new("main", "session-1"));
            lane.session_file = Some(session_file.clone());
            lane.active_run_id = Some("run-v2".into());
        }
        supervisor
            .append_tool_started_record(
                "session-1",
                "main",
                &session_file,
                "call-v2",
                "view_file",
                serde_json::json!({}),
            )
            .unwrap();
        let mut store = JsonlStore::open(&session_file).unwrap();
        let result_seq = store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(store.records().iter().map(HarnessRecord::seq))
            .max()
            .unwrap()
            + 1;
        store
            .append_entry(threadlane_agent::harness::Entry {
                id: "result-call-v2".into(),
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
                terminate: false,
            })
            .unwrap();
        supervisor
            .append_tool_finished_record("session-1", "main", &session_file, "call-v2", false)
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

        supervisor
            .append_persisted_lane_record(
                "session-1",
                "main",
                &session_file,
                OpRecord::OperationStarted {
                    id: "run-1".into(),
                    seq: 1,
                    lane: "main".into(),
                    timestamp: 1,
                    source_leaf_id: None,
                    kind: "prompt".into(),
                    system_prompt_override: None,
                },
            )
            .unwrap();

        let harness = JsonlStore::open(&session_file).unwrap();
        assert!(harness.records().iter().any(|record| {
            matches!(record, HarnessRecord::OperationStarted { id, .. } if id == "run-1")
        }));
        assert_eq!(
            supervisor
                .get_or_create_lane("session-1", "main")
                .op_log
                .len(),
            1
        );

        let mut tree = threadlane_agent::SessionTree::new("session-1");
        let recovery = supervisor
            .restore_session_lanes("session-1", &session_file, &mut tree)
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
                threadlane_agent::OpOutcome::Aborted,
            )
            .unwrap();
        let mut tree = threadlane_agent::SessionTree::new("session-1");
        let second_recovery = supervisor
            .restore_session_lanes("session-1", &session_file, &mut tree)
            .unwrap();
        assert_eq!(second_recovery.recovered_open_operations, 0);
    }

    #[test]
    fn v2_only_open_run_is_restored() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        let mut harness = AgentHarness::new(JsonlStore::open(&session_file).unwrap());
        harness
            .accept_prompt("run-v2", AgentMessage::user("prompt", Vec::new()))
            .unwrap();
        harness.drive_one().unwrap();
        drop(harness);

        let mut tree = threadlane_agent::SessionTree::new("session-1");
        let recovery = supervisor
            .restore_session_lanes("session-1", &session_file, &mut tree)
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
        let mut store = JsonlStore::open(&session_file).unwrap();
        store
            .append_entry(threadlane_agent::harness::Entry {
                id: "subagent-root".into(),
                parent_id: None,
                lane: "subagent-1".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::user("inspect", Vec::new()),
                terminate: false,
            })
            .unwrap();
        let mut harness = AgentHarness::new(store);
        harness
            .accept_prompt_on_lane(
                "subagent-1",
                "run-subagent",
                AgentMessage::user("inspect", Vec::new()),
            )
            .unwrap();
        harness.drive_to_completion().unwrap();
        harness
            .enqueue_unbound_on_lane(
                "subagent-1",
                HarnessQueueKind::FollowUp,
                ProvisionedEntry {
                    id: "queued-follow-up".into(),
                    parent_id: None,
                    message: AgentMessage::user("follow up", Vec::new()),
                },
            )
            .unwrap();
        harness.drive_to_completion().unwrap();
        let mut store = JsonlStore::open(&session_file).unwrap();
        let queue_seq = store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(store.records().iter().map(HarnessRecord::seq))
            .max()
            .unwrap()
            + 1;
        store
            .append_record(HarnessRecord::QueueEnqueued {
                id: "queue-steer-high".into(),
                seq: queue_seq,
                lane: "subagent-1".into(),
                timestamp: queue_seq,
                run_id: Some("run-subagent".into()),
                queue: HarnessQueueKind::Steer,
                priority: Some(threadlane_agent::SteerPriority::High),
                target: ProvisionedEntry {
                    id: "queued-steer-high".into(),
                    parent_id: None,
                    message: AgentMessage::user("urgent", Vec::new()),
                },
            })
            .unwrap();

        let mut tree = threadlane_agent::SessionTree::new("session-1");
        let recovery = supervisor
            .restore_session_lanes("session-1", &session_file, &mut tree)
            .unwrap();
        assert_eq!(recovery.open_operation_ids, vec!["run-subagent"]);
        let lane = supervisor.get_or_create_lane("session-1", "subagent-1");
        assert_eq!(lane.status, LaneStatus::Suspended);
        assert_eq!(lane.active_run_id.as_deref(), Some("run-subagent"));
        assert_eq!(lane.queue.follow_up.len(), 1);
        assert_eq!(lane.queue.steer.len(), 1);
        assert_eq!(
            lane.queue.steer[0].priority,
            threadlane_agent::SteerPriority::High
        );
    }

    #[test]
    fn supervisor_lane_navigation_and_deferred_redemption() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let mut tree = threadlane_agent::SessionTree::new("session-test");

        let root_id = tree.add_message(AgentMessage::User {
            content: "root prompt".into(),
        });
        let child_id = tree.add_message(AgentMessage::Assistant {
            content: Some("reply".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });

        supervisor.update_lane_leaf("session-test", "main", Some(child_id.clone()));
        assert!(supervisor
            .navigate_lane("session-test", "main", &root_id, &mut tree)
            .unwrap());

        let lane = supervisor.get_or_create_lane("session-test", "main");
        assert_eq!(lane.leaf_id.as_deref(), Some(root_id.as_str()));

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
                &mut tree,
            )
            .unwrap();

        let branch = tree.get_branch_messages(Some(&redeemed_id));
        assert_eq!(branch.len(), 2);
    }

    #[test]
    fn v2_lane_navigation_persists_typed_navigation_records() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        let session_file = dir.path().join("session.jsonl");
        let mut tree = threadlane_agent::SessionTree::new("session-test");
        let root_id = tree.add_message(AgentMessage::user("root", Vec::new()));
        let child_id = tree.add_message(AgentMessage::Assistant {
            content: Some("child".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });
        let mut store = JsonlStore::open({
            fs::File::create(&session_file).unwrap();
            &session_file
        })
        .unwrap();
        store
            .append_entry(threadlane_agent::harness::Entry {
                id: root_id.clone(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::user("root", Vec::new()),
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_agent::harness::Entry {
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
            .navigate_lane("session-test", "main", &root_id, &mut tree)
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
    fn subagent_events_create_sibling_supervisor_lanes() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        supervisor.get_or_create_lane("session-1", "main");
        for task_index in 0..2 {
            supervisor.observe_session_event(
                "project-1",
                "session-1",
                None,
                &AgentEvent::SubagentQueued {
                    run_id: 9,
                    task_index,
                    agent: "worker".into(),
                    task: format!("task {task_index}"),
                },
            );
        }

        for task_index in 0..2 {
            let lane =
                supervisor.get_or_create_lane("session-1", &format!("subagent-9:{task_index}"));
            assert_eq!(lane.parent_lane.as_deref(), Some("main"));
        }
        assert_eq!(supervisor.cancel_lane_hierarchy("session-1", "main"), 3);
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
}
