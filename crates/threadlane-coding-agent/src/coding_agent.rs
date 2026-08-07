use crate::agents::{discover_agents, AgentConfig, AgentScope};
use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::context::ProjectContext;
use crate::extension_broker::{
    BrokerError, BrokerRequest, CapabilityDispatcher, CapabilityHandler, BROKER_API_VERSION,
};
use crate::mcp::{McpManager, McpToolExecutor};
use crate::packages::default_global_threadlane_dir;
use crate::plan::{SessionPlanStore, UpdatePlanToolExecutor};
use crate::skills::{LoadSkillToolExecutor, SkillManager, SkillRegistry};
use crate::system_prompt::{build_system_prompt, SystemPromptBuildOptions, SystemPromptConfig};
use crate::wasi_extension::{WasiExtensionManager, WasiLegacyEffect};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use threadlane_agent::harness::{
    AgentHarness, DeferredResolution, Entry as HarnessEntry, EventError, HarnessEvent,
    HarnessEventHub, HookContext, HookKind, JsonlStore, OperationIntent, OperationOutcome,
    QueueKind, Record as HarnessRecord, Reducer, RetryPolicy, SessionIdGenerator, SessionStore,
    Snapshot, Subscription, ToolRecovery, ToolReplaySafety as HarnessToolReplaySafety,
    ToolResult as HarnessToolResult, ToolSpec,
};
use threadlane_agent::{
    repair_interrupted_tool_turn, AfterToolCallHook, AfterToolCallResult, Agent, AgentEvent,
    AgentMessage, AgentState, AgentToolCall, AgentToolDefinition, AgentToolResult,
    BeforeToolCallHook, BeforeToolCallResult, ImageAttachment, OpOutcome, OpRecord,
    ReasoningEffort, SessionTree, SubagentRecoveryStatus, TokenUsage, ToolExecutor,
};
use threadlane_provider::openai::fetch_available_models;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;
use tokio::time::{timeout, Duration};

const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CAPABILITY_BUFFER_BYTES: usize = 64 * 1024;
const MAX_PROCESS_TIMEOUT_MS: u64 = 120_000;
const MAX_PROCESS_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_MANAGED_PROCESSES: usize = 16;
const DEFAULT_RECV_TIMEOUT_MS: u64 = 5000;
const MAX_RECV_TIMEOUT_MS: u64 = 30_000;
const MAX_MANAGED_STDOUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_BROKER_CONTINUATION_ROUNDS: usize = 4;
const MAX_SUBAGENT_TASKS: usize = 8;
const MAX_SUBAGENT_TASK_CHARS: usize = 32_000;
const SUBAGENT_CONCURRENCY_LIMIT: usize = 4;
const SUBAGENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SUBAGENT_RECOVERY_PROMPT: &str =
    "Continue from the recovered checkpoint and finish the assigned task.";

fn is_retryable_generation_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "timeout",
        "timed out",
        "connection reset",
        "connection refused",
        "temporarily unavailable",
        "rate limit",
        "status 429",
        "status 502",
        "status 503",
        "status 504",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}

fn generation_event_drain_error(error: broadcast::error::TryRecvError) -> Option<&'static str> {
    match error {
        broadcast::error::TryRecvError::Lagged(_) => None,
        broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed => {
            Some("generation ended without a durable AgentEnd event")
        }
    }
}
static NEXT_SUBAGENT_UI_RUN_ID: AtomicU64 = AtomicU64::new(1);

type AgentRunner = Arc<
    dyn Fn(
            Vec<AgentRunTask>,
            bool,
            Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

#[cfg(test)]
type AgentWorkObserver = Arc<std::sync::Mutex<Vec<AgentWork>>>;
#[cfg(test)]
type SubagentObserverState = Arc<std::sync::Mutex<Option<AgentWorkObserver>>>;
#[cfg(test)]
type SubagentBoundaryObserver = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
struct SubagentRunContext {
    api_key: String,
    account_id: Option<String>,
    parent_model: String,
    parent_session_id: String,
    work_dir: PathBuf,
    extensions: Arc<WasiExtensionManager>,
    parent_event_tx: broadcast::Sender<AgentEvent>,
    parent_leaf_id: Option<String>,
    session_file: Option<PathBuf>,
    #[cfg(test)]
    scheduler_observer: Option<AgentWorkObserver>,
    #[cfg(test)]
    child_work_observer: Option<SubagentBoundaryObserver>,
    #[cfg(test)]
    child_tool_observer: Option<Arc<AtomicBool>>,
    semaphore: Arc<tokio::sync::Semaphore>,
}

use crate::policy::ToolPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentWork {
    RequestTurn(String),
    SteerMessage {
        content: String,
        images: Vec<ImageAttachment>,
    },
    NextRunMessage {
        content: String,
        images: Vec<ImageAttachment>,
    },
    QueueMessage {
        content: String,
        images: Vec<ImageAttachment>,
    },
}

fn harness_next_seq(store: &JsonlStore) -> u64 {
    store
        .entries()
        .iter()
        .map(|entry| entry.seq)
        .chain(store.records().iter().map(HarnessRecord::seq))
        .max()
        .unwrap_or(0)
        + 1
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn enqueue_harness_queue(
    session_file: &Path,
    queue: QueueKind,
    content: String,
    images: Vec<ImageAttachment>,
) -> Result<String, String> {
    let store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
    let mut harness = AgentHarness::new(store);
    let state = Reducer::reduce(harness.store()).map_err(|error| error.to_string())?;
    let lane = state
        .lane("main")
        .ok_or_else(|| "main harness lane is unavailable".to_string())?;
    let parent_id = lane.leaf_id.clone();
    let seq = harness_next_seq(harness.store());
    let entry_id = format!("queued-{seq}");
    harness
        .enqueue_unbound(
            queue,
            threadlane_agent::harness::ProvisionedEntry {
                id: entry_id.clone(),
                parent_id,
                message: AgentMessage::user(content, images),
            },
        )
        .map_err(|error| error.to_string())?;
    harness
        .drive_to_completion()
        .map_err(|error| error.to_string())?;
    Ok(entry_id)
}

fn enqueue_harness_follow_up(
    session_file: &Path,
    content: String,
    images: Vec<ImageAttachment>,
) -> Result<String, String> {
    enqueue_harness_queue(session_file, QueueKind::FollowUp, content, images)
}

fn consume_harness_queue(session_file: &Path, queue: QueueKind) -> Result<(), String> {
    let store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
    let mut harness = AgentHarness::new(store);
    let state = Reducer::reduce(harness.store()).map_err(|error| error.to_string())?;
    let queued = state
        .lane("main")
        .map(|lane| {
            lane.queued
                .iter()
                .filter(|entry| entry.queue == queue)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for entry in queued {
        harness
            .consume_unbound(&entry.target.id)
            .map_err(|error| error.to_string())?;
    }
    harness
        .drive_to_completion()
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn consume_harness_follow_ups(session_file: &Path) -> Result<(), String> {
    consume_harness_queue(session_file, QueueKind::FollowUp)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunTask {
    agent: String,
    task: String,
    instructions: Option<String>,
    tools: Option<Vec<String>>,
    model: Option<String>,
}

#[derive(Clone, Debug)]
enum SubagentLaneStatus {
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
struct CompletedSubagentLane {
    lane_name: String,
    run_id: String,
    parent_leaf_id: Option<String>,
    task: String,
    agent: String,
    status: SubagentLaneStatus,
    messages: Vec<AgentMessage>,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SubagentLaneIdentity {
    lane_name: String,
    run_id: String,
    source_leaf_id: Option<String>,
    started_seq: u64,
}

#[derive(Debug)]
struct SubagentStartError {
    identity: Option<SubagentLaneIdentity>,
    error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum InterruptedSubagentRecoveryState {
    Pending,
    Complete,
}

fn recover_v2_subagent_records(session_file: &Path) -> Result<Vec<OpRecord>, String> {
    let store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
    let mut records = Vec::new();
    for record in store.records() {
        if record.lane() == "main" {
            continue;
        }
        let recovered = match record {
            HarnessRecord::OperationStarted {
                id,
                seq,
                lane,
                timestamp,
                source_leaf_id,
                ..
            } => OpRecord::OperationStarted {
                id: id.clone(),
                seq: *seq,
                lane: lane.clone(),
                timestamp: *timestamp,
                source_leaf_id: source_leaf_id.clone(),
                kind: "subagent".into(),
                system_prompt_override: None,
            },
            HarnessRecord::AbortRequested {
                id,
                seq,
                lane,
                timestamp,
                run_id,
            } => OpRecord::AbortRequested {
                id: id.clone(),
                seq: *seq,
                lane: lane.clone(),
                timestamp: *timestamp,
                run_id: run_id.clone(),
            },
            HarnessRecord::OperationFinished {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                outcome,
                error,
            } => OpRecord::OperationFinished {
                id: id.clone(),
                seq: *seq,
                lane: lane.clone(),
                timestamp: *timestamp,
                run_id: run_id.clone(),
                outcome: match outcome {
                    OperationOutcome::Completed => OpOutcome::Completed,
                    OperationOutcome::Aborted => OpOutcome::Aborted,
                    OperationOutcome::Failed => OpOutcome::Failed,
                    OperationOutcome::Declined => OpOutcome::Declined,
                },
                error: error.clone(),
            },
            HarnessRecord::StepAttempt {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                attempt,
                ..
            } => OpRecord::TaskAttempt {
                id: id.clone(),
                seq: *seq,
                lane: lane.clone(),
                timestamp: *timestamp,
                run_id: run_id.clone(),
                task: String::new(),
                attempt: *attempt,
            },
            HarnessRecord::ToolStarted {
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
            } => OpRecord::ToolStarted {
                id: id.clone(),
                seq: *seq,
                lane: lane.clone(),
                timestamp: *timestamp,
                run_id: run_id.clone(),
                assistant_entry_id: assistant_entry_id.clone(),
                tool_index: *tool_index,
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                effective_args: effective_args.clone(),
                result_entry_id: result_entry_id.clone(),
                replay: match replay {
                    HarnessToolReplaySafety::Safe => threadlane_agent::ToolReplaySafety::Safe,
                    HarnessToolReplaySafety::Never => threadlane_agent::ToolReplaySafety::Never,
                },
            },
            HarnessRecord::ToolFinished {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                tool_call_id,
                result_entry_id,
                terminate,
            } => OpRecord::ToolFinished {
                id: id.clone(),
                seq: *seq,
                lane: lane.clone(),
                timestamp: *timestamp,
                run_id: run_id.clone(),
                tool_call_id: tool_call_id.clone(),
                result_entry_id: result_entry_id.clone(),
                terminate: *terminate,
            },
            HarnessRecord::LaneMoved {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                target_leaf_id,
            } => OpRecord::Navigation {
                id: id.clone(),
                seq: *seq,
                lane: lane.clone(),
                timestamp: *timestamp,
                run_id: run_id.clone(),
                target_id: target_leaf_id.clone(),
                summary_entry_id: None,
            },
            HarnessRecord::WriteDeferred {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                target,
            } => OpRecord::WriteDeferred {
                id: id.clone(),
                seq: *seq,
                lane: lane.clone(),
                timestamp: *timestamp,
                run_id: run_id.clone(),
                target: target.message.clone(),
            },
            _ => continue,
        };
        records.push(recovered);
    }
    let open_runs = records
        .iter()
        .filter_map(|record| match record {
            OpRecord::OperationStarted { lane, id, .. } => Some((lane.clone(), id.clone())),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut checkpoint_messages = HashMap::<String, Vec<AgentMessage>>::new();
    for entry in store.entries() {
        if entry.lane == "main" {
            continue;
        }
        if let Some(run_id) = open_runs.get(&entry.lane) {
            checkpoint_messages
                .entry(run_id.clone())
                .or_default()
                .push(entry.message.clone());
            records.push(OpRecord::WriteDeferred {
                id: entry.id.clone(),
                seq: entry.seq,
                lane: entry.lane.clone(),
                timestamp: entry.timestamp,
                run_id: run_id.clone(),
                target: entry.message.clone(),
            });
        }
    }
    for (run_id, messages) in checkpoint_messages {
        let has_checkpoint = messages
            .iter()
            .any(|message| matches!(message, AgentMessage::Assistant { .. }))
            || messages
                .iter()
                .filter(|message| matches!(message, AgentMessage::User { .. }))
                .count()
                > 1;
        if has_checkpoint {
            let lane = records
                .iter()
                .find_map(|record| match record {
                    OpRecord::OperationStarted { id, lane, .. } if id == &run_id => {
                        Some(lane.clone())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let task = messages.iter().rev().find_map(|message| match message {
                AgentMessage::User { content } => Some(content.clone()),
                _ => None,
            });
            if let Some(task) = task {
                records.push(OpRecord::TaskAttempt {
                    id: format!("task-attempt-{run_id}-recovered"),
                    seq: records.iter().map(OpRecord::seq).max().unwrap_or(0) + 1,
                    lane,
                    timestamp: timestamp(),
                    run_id,
                    task,
                    attempt: 1,
                });
            }
        }
    }
    records.sort_by_key(OpRecord::seq);
    Ok(records)
}

#[derive(Clone, Debug, Default)]
pub struct SubagentCancellationGuard;

struct ActiveRun {
    id: u64,
    handle: tokio::task::AbortHandle,
}

#[derive(Default)]
struct ActiveRunState {
    next_id: u64,
    active: Option<ActiveRun>,
    cancellation_guard: Option<SubagentCancellationGuard>,
}

#[derive(Clone)]
pub struct CodingAgentCancellation {
    state: Arc<std::sync::Mutex<ActiveRunState>>,
    harness_session_file: Option<PathBuf>,
    event_tx: broadcast::Sender<AgentEvent>,
}

impl CodingAgentCancellation {
    pub fn track_active_run(&self, handle: tokio::task::AbortHandle) -> Result<u64, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        if state.active.is_some() {
            return Err("A generation is already running".into());
        }
        state.next_id = state.next_id.wrapping_add(1);
        let id = state.next_id;
        state.active = Some(ActiveRun { id, handle });
        Ok(id)
    }

    pub fn finish_active_run(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            if state.active.as_ref().is_some_and(|active| active.id == id) {
                state.active = None;
            }
        }
    }

    pub fn clear_cancellation_guard(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cancellation_guard = None;
        }
        if let Some(path) = self.harness_session_file.as_deref() {
            harness_cancellation_state(path).store(false, Ordering::SeqCst);
        }
    }

    pub fn cancel(&self) -> Result<(), String> {
        if let Some(path) = self.harness_session_file.as_deref() {
            let mut journal = HarnessJournal::open(path)?;
            let _ = journal.request_abort();
        }
        let handle = {
            let mut state = self.state.lock().map_err(|error| error.to_string())?;
            state.active.take().map(|active| active.handle)
        };
        if let Some(handle) = handle {
            handle.abort();
        }
        let _ = self.event_tx.send(AgentEvent::AgentError {
            error: "Generation cancelled".into(),
        });
        Ok(())
    }
}

pub(crate) fn abort_open_subagent_operations(
    session_file: &Path,
) -> Result<SubagentCancellationGuard, String> {
    cancel_open_subagent_operations(session_file)?;
    Ok(SubagentCancellationGuard)
}

pub fn cancel_open_subagent_operations(session_file: &Path) -> Result<(), String> {
    if session_file.exists() {
        let mut journal = HarnessJournal::open(session_file)?;
        journal.refresh()?;
        let open_runs = Reducer::reduce(&journal.store)
            .map_err(|error| error.to_string())?
            .lanes
            .into_iter()
            .filter(|lane| lane.name != "main")
            .filter_map(|lane| lane.open_operation)
            .collect::<Vec<_>>();
        for run_id in open_runs {
            journal
                .store
                .finish_operation(
                    &run_id,
                    OperationOutcome::Aborted,
                    Some("Generation cancelled".into()),
                )
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
struct AgentWorkScheduler {
    pending: Arc<std::sync::Mutex<Vec<AgentWork>>>,
    #[cfg(test)]
    test_observer: SubagentObserverState,
}

impl AgentWorkScheduler {
    fn schedule(&self, work: AgentWork) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push(work);
        }
    }

    fn drain(&self) -> Vec<AgentWork> {
        self.pending
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn set_test_observer(&self, observer: Arc<std::sync::Mutex<Vec<AgentWork>>>) {
        if let Ok(mut current) = self.test_observer.lock() {
            *current = Some(observer);
        }
    }

    async fn run(&self, agent: &mut Agent) -> bool {
        let pending = self.drain();
        if pending.is_empty() {
            return false;
        }
        #[cfg(test)]
        if let Ok(Some(observer)) = self.test_observer.lock().map(|observer| observer.clone()) {
            if let Ok(mut observed) = observer.lock() {
                observed.extend(pending);
            }
            return true;
        }
        for work in pending {
            match work {
                AgentWork::RequestTurn(prompt) => agent.prompt(&prompt).await,
                AgentWork::SteerMessage { content, images } => {
                    agent.steer(AgentMessage::user(content, images));
                    agent.run_steer().await;
                }
                AgentWork::NextRunMessage { content, images } => {
                    agent.follow_up(AgentMessage::user(content, images));
                    agent.run_follow_up().await;
                }
                AgentWork::QueueMessage { content, images } => {
                    agent.follow_up(AgentMessage::user(content, images));
                    agent.run_follow_up().await;
                }
            }
        }
        true
    }
}

#[cfg(test)]
struct DeterministicSubagentToolExecutor {
    observed: Arc<AtomicBool>,
}

#[cfg(test)]
#[async_trait]
impl ToolExecutor for DeterministicSubagentToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.test.subagent_tool"
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        vec![AgentToolDefinition {
            name: "test_child_tool".into(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
            strict: None,
        }]
    }

    async fn execute_tool(&self, name: &str, _args: &str) -> Option<Result<String, String>> {
        (name == "test_child_tool").then(|| {
            self.observed.store(true, Ordering::SeqCst);
            Ok("test child tool result".into())
        })
    }
}

#[derive(Clone)]
pub struct CodingAgentWorkHandle {
    scheduler: AgentWorkScheduler,
    session_file: Option<PathBuf>,
}

impl CodingAgentWorkHandle {
    pub fn queue_follow_up(&self, content: impl Into<String>) {
        self.queue_follow_up_with_images(content, Vec::new());
    }

    pub fn queue_follow_up_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            if let Err(error) = enqueue_harness_follow_up(path, content.clone(), images.clone()) {
                eprintln!("Failed to persist queued follow-up: {error}");
                return;
            }
        }
        self.scheduler
            .schedule(AgentWork::QueueMessage { content, images });
    }

    pub fn queue_steer_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            enqueue_harness_queue(path, QueueKind::Steer, content.clone(), images.clone())?;
        }
        self.scheduler
            .schedule(AgentWork::SteerMessage { content, images });
        Ok(())
    }

    pub fn queue_next_run_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            enqueue_harness_queue(path, QueueKind::NextRun, content.clone(), images.clone())?;
        }
        self.scheduler
            .schedule(AgentWork::NextRunMessage { content, images });
        Ok(())
    }

    pub fn try_queue_follow_up_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            enqueue_harness_follow_up(path, content.clone(), images.clone())?;
        }
        self.scheduler
            .schedule(AgentWork::QueueMessage { content, images });
        Ok(())
    }

    pub fn cancel_queued_follow_up(&self, entry_id: &str) -> Result<(), String> {
        let Some(path) = self.session_file.as_deref() else {
            return Err("session persistence is unavailable".into());
        };
        let mut harness =
            AgentHarness::new(JsonlStore::open(path).map_err(|error| error.to_string())?);
        harness
            .cancel_unbound(entry_id)
            .map_err(|error| error.to_string())?;
        harness
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }
}

/// A persistent subprocess managed by the host for WASI extensions.
/// Extensions reference managed processes by name across invocations.
struct ManagedProcess {
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    stdout_buf: Arc<tokio::sync::Mutex<Vec<u8>>>,
    pid: u32,
    alive: Arc<AtomicBool>,
}

#[derive(Hash, Eq, PartialEq)]
struct ManagedProcessKey {
    extension: String,
    session: Option<String>,
    name: String,
}
type ManagedProcessRegistry = Arc<tokio::sync::Mutex<HashMap<ManagedProcessKey, ManagedProcess>>>;

struct HostCapabilityHandler {
    capability: &'static str,
    tool_policy: Option<Arc<tokio::sync::Mutex<ToolPolicy>>>,
    extensions: Arc<WasiExtensionManager>,
    work_dir: PathBuf,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    allowed_hosts: Arc<HashSet<String>>,
    agent_work: AgentWorkScheduler,
    agent_runner: Option<AgentRunner>,
    persist_tool_policy: bool,
    managed_processes: ManagedProcessRegistry,
}

impl HostCapabilityHandler {
    fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        self.handle_for_extension(request, "")
    }

    fn handle_for_extension(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        match self.capability {
            "tools" => self.handle_tools(request),
            "agent" => self.handle_agent(request),
            "session" => self.handle_session(request, invoking_extension),
            "fs" => self.handle_fs(request),
            "process" | "network" => Err(BrokerError {
                code: "async_required".into(),
                message: format!("Capability `{}` requires async dispatch", self.capability),
            }),
            "ui" => self.handle_ui(request),
            "events" => self.handle_events(request, invoking_extension),
            _ => Err(BrokerError {
                code: "unknown_capability".into(),
                message: format!("Host does not implement capability `{}`", self.capability),
            }),
        }
    }
}

#[async_trait]
impl CapabilityHandler for HostCapabilityHandler {
    fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        HostCapabilityHandler::handle(self, request)
    }

    fn handle_for_extension(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        HostCapabilityHandler::handle_for_extension(self, request, invoking_extension)
    }

    async fn handle_for_extension_async(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        match self.capability {
            "agent" if request.operation == "run" => self.handle_agent_run_async(request).await,
            "process" => self.handle_process_async(request, invoking_extension).await,
            "network" => self.handle_network_async(request).await,
            _ => self.handle_for_extension(request, invoking_extension),
        }
    }
}

impl HostCapabilityHandler {
    fn handle_tools(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let policy = self
            .tool_policy
            .as_ref()
            .ok_or_else(|| internal_error("Tool policy unavailable"))?;
        match request.operation.as_str() {
            "set_policy" => {
                let value = string_argument(&request.arguments, "policy")?;
                let next = match value {
                    "read_only" => ToolPolicy::ReadOnly,
                    "full" => ToolPolicy::FullAccess,
                    _ => return Err(invalid_argument("policy must be `read_only` or `full`")),
                };
                if self.persist_tool_policy {
                    self.extensions
                        .set_host_state("tools.policy", Value::String(value.into()))
                        .map_err(host_error)?;
                }
                let mut current = policy
                    .try_lock()
                    .map_err(|_| internal_error("Tool policy is busy"))?;
                *current = next;
                Ok(Value::Null)
            }
            "get_policy" => {
                let current = policy
                    .try_lock()
                    .map_err(|_| internal_error("Tool policy is busy"))?;
                Ok(serde_json::json!({"message": match *current {
                    ToolPolicy::ReadOnly => "read_only",
                    ToolPolicy::FullAccess => "full",
                }}))
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    async fn handle_agent_run_async(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let values = request
            .arguments
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_argument("missing argument `tasks`"))?;
        if values.len() > MAX_SUBAGENT_TASKS {
            return Err(invalid_argument(format!(
                "agent.run accepts at most {MAX_SUBAGENT_TASKS} tasks"
            )));
        }
        let tasks = values
            .iter()
            .map(|value| {
                let agent = value
                    .get("agent")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|agent| !agent.is_empty())
                    .ok_or_else(|| invalid_argument("each task requires a non-empty `agent`"))?;
                let task = value
                    .get("task")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|task| !task.is_empty())
                    .ok_or_else(|| invalid_argument("each task requires a non-empty `task`"))?;
                if agent.chars().count() > 128 || task.chars().count() > MAX_SUBAGENT_TASK_CHARS {
                    return Err(invalid_argument("agent.run task fields exceed size limits"));
                }
                let instructions = value
                    .get("instructions")
                    .or_else(|| value.get("system_prompt"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let tools = value.get("tools").and_then(Value::as_array).map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                });
                let model = value
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                Ok(AgentRunTask {
                    agent: agent.into(),
                    task: task.into(),
                    instructions,
                    tools,
                    model,
                })
            })
            .collect::<Result<Vec<_>, BrokerError>>()?;
        if tasks.is_empty() {
            return Err(invalid_argument("agent.run requires at least one task"));
        }
        let parallel = request
            .arguments
            .get("parallel")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let runner = self
            .agent_runner
            .as_ref()
            .ok_or_else(|| internal_error("Child-agent runner unavailable"))?;
        (runner)(tasks, parallel, None).await.map_err(host_error)
    }

    fn handle_agent(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let work = match request.operation.as_str() {
            "request_turn" => {
                AgentWork::RequestTurn(string_argument(&request.arguments, "prompt")?.to_string())
            }
            "queue_message" => AgentWork::QueueMessage {
                content: string_argument(&request.arguments, "content")?.to_string(),
                images: Vec::new(),
            },
            _ => return unknown_operation(self.capability, &request.operation),
        };
        self.agent_work.schedule(work);
        Ok(serde_json::json!({"queued": true}))
    }

    fn handle_session(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        if invoking_extension.is_empty() {
            return Err(invalid_argument(
                "session capability requires host extension identity",
            ));
        }
        match request.operation.as_str() {
            "get_extension_state" => Ok(serde_json::json!({
                "message": self.extensions.extension_state(invoking_extension)
                    .unwrap_or_else(|| serde_json::json!({}))
                    .to_string()
            })),
            "set_extension_state" => {
                let state = request
                    .arguments
                    .get("state")
                    .cloned()
                    .ok_or_else(|| invalid_argument("missing argument `state`"))?;
                self.extensions
                    .set_extension_state(invoking_extension, state)
                    .map_err(host_error)?;
                Ok(Value::Null)
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    fn handle_fs(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        match request.operation.as_str() {
            "read_text" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                let text = fs::read_to_string(path).map_err(host_error)?;
                Ok(serde_json::json!({"message": text}))
            }
            "absolute_path" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                Ok(serde_json::json!({"message": path.to_string_lossy()}))
            }
            "write_text" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                let content = string_argument(&request.arguments, "content")?;
                fs::write(path, content).map_err(host_error)?;
                Ok(Value::Null)
            }
            "rename" => {
                let from = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "from")?,
                )?;
                let to =
                    resolve_work_path(&self.work_dir, string_argument(&request.arguments, "to")?)?;
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent).map_err(host_error)?;
                }
                fs::rename(from, to).map_err(host_error)?;
                Ok(Value::Null)
            }
            "list" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                let entries = fs::read_dir(path)
                    .map_err(host_error)?
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                Ok(
                    serde_json::json!({"message": serde_json::to_string(&entries).unwrap_or_default()}),
                )
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    fn managed_process_key(
        &self,
        name: &str,
        invoking_extension: &str,
    ) -> Result<ManagedProcessKey, BrokerError> {
        if invoking_extension.is_empty() {
            return Err(invalid_argument(
                "process capability requires host extension identity",
            ));
        }
        Ok(ManagedProcessKey {
            extension: invoking_extension.into(),
            session: self.extensions.active_session_scope().map_err(host_error)?,
            name: name.into(),
        })
    }
    async fn handle_process_async(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        match request.operation.as_str() {
            "run" => self.handle_process_run(request).await,
            "spawn" => self.handle_process_spawn(request, invoking_extension).await,
            "send" => self.handle_process_send(request, invoking_extension).await,
            "recv" => self.handle_process_recv(request, invoking_extension).await,
            "kill" => self.handle_process_kill(request, invoking_extension).await,
            "status" => {
                self.handle_process_status(request, invoking_extension)
                    .await
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    /// Original fire-and-forget process execution.
    async fn handle_process_run(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let program = string_argument(&request.arguments, "program")?;
        let limits = process_run_limits(&request.arguments)?;
        let args = request
            .arguments
            .get("args")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_argument("missing argument `args`"))?;
        let cwd = request
            .arguments
            .get("cwd")
            .and_then(Value::as_str)
            .map(|cwd| resolve_work_path(&self.work_dir, cwd))
            .transpose()?;
        let mut command = tokio::process::Command::new(program);
        command
            .current_dir(cwd.unwrap_or_else(|| self.work_dir.clone()))
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for arg in args {
            command.arg(
                arg.as_str()
                    .ok_or_else(|| invalid_argument("args must be strings"))?,
            );
        }
        let mut child = command.spawn().map_err(host_error)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| internal_error("process stdout pipe unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| internal_error("process stderr pipe unavailable"))?;
        let (stdout, stderr, status) = timeout(limits.timeout, async {
            tokio::try_join!(
                read_limited(
                    stdout,
                    "process_output_too_large",
                    "process stdout",
                    limits.max_output_bytes,
                ),
                read_limited(
                    stderr,
                    "process_output_too_large",
                    "process stderr",
                    limits.max_output_bytes,
                ),
                async { child.wait().await.map_err(host_error) },
            )
        })
        .await
        .map_err(|_| timeout_error("process.run"))??;
        let stdout =
            String::from_utf8(stdout).map_err(|_| invalid_argument("stdout was not UTF-8"))?;
        let stderr =
            String::from_utf8(stderr).map_err(|_| invalid_argument("stderr was not UTF-8"))?;
        Ok(serde_json::json!({"message": serde_json::json!({
            "exit_code": status.code(), "stdout": stdout, "stderr": stderr
        }).to_string()}))
    }

    /// Spawn a named persistent subprocess with piped stdin/stdout.
    async fn handle_process_spawn(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = string_argument(&request.arguments, "name")?;
        let key = self.managed_process_key(name, invoking_extension)?;
        let program = string_argument(&request.arguments, "program")?;
        let args_val = request
            .arguments
            .get("args")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut registry = self.managed_processes.lock().await;

        // Idempotent: if a process with this name is alive, return its info.
        if let Some(existing) = registry.get(&key) {
            if existing.alive.load(Ordering::Relaxed) {
                return Ok(serde_json::json!({"message": serde_json::json!({
                    "name": name, "pid": existing.pid
                }).to_string()}));
            }
            // Dead process — remove and re-spawn below.
            registry.remove(&key);
        }

        if registry.len() >= MAX_MANAGED_PROCESSES {
            return Err(BrokerError {
                code: "limit_exceeded".into(),
                message: format!(
                    "Cannot spawn more than {MAX_MANAGED_PROCESSES} managed processes"
                ),
            });
        }

        let cwd = request
            .arguments
            .get("cwd")
            .and_then(Value::as_str)
            .map(|cwd| resolve_work_path(&self.work_dir, cwd))
            .transpose()?;
        let mut command = tokio::process::Command::new(program);
        command
            .current_dir(cwd.unwrap_or_else(|| self.work_dir.clone()))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for arg in &args_val {
            command.arg(
                arg.as_str()
                    .ok_or_else(|| invalid_argument("args must be strings"))?,
            );
        }

        let child = Arc::new(tokio::sync::Mutex::new(
            command.spawn().map_err(host_error)?,
        ));
        let pid = child.lock().await.id().unwrap_or(0);
        let mut child_guard = child.lock().await;
        let stdin = child_guard
            .stdin
            .take()
            .ok_or_else(|| internal_error("process stdin pipe unavailable"))?;
        let mut stdout = child_guard
            .stdout
            .take()
            .ok_or_else(|| internal_error("process stdout pipe unavailable"))?;
        drop(child_guard);

        let stdout_buf: Arc<tokio::sync::Mutex<Vec<u8>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let alive = Arc::new(AtomicBool::new(true));

        // Background reader: continuously reads stdout into the shared buffer.
        let buf_clone = stdout_buf.clone();
        let alive_clone = alive.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut buf = buf_clone.lock().await;
                        if buf.len() + n > MAX_MANAGED_STDOUT_BYTES {
                            // Prevent unbounded growth: drop oldest data.
                            let overflow = (buf.len() + n) - MAX_MANAGED_STDOUT_BYTES;
                            buf.drain(..overflow);
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    Err(_) => break,
                }
            }
            alive_clone.store(false, Ordering::Relaxed);
        });

        registry.insert(
            key,
            ManagedProcess {
                child,
                stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
                stdout_buf,
                pid,
                alive,
            },
        );

        Ok(serde_json::json!({"message": serde_json::json!({
            "name": name, "pid": pid
        }).to_string()}))
    }

    /// Write data to a named process's stdin.
    async fn handle_process_send(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = string_argument(&request.arguments, "name")?;
        let data = string_argument(&request.arguments, "data")?;
        let key = self.managed_process_key(name, invoking_extension)?;

        let stdin = {
            let registry = self.managed_processes.lock().await;
            let process = registry.get(&key).ok_or_else(|| BrokerError {
                code: "not_found".into(),
                message: format!("No managed process named `{name}`"),
            })?;
            if !process.alive.load(Ordering::Relaxed) {
                return Err(BrokerError {
                    code: "process_dead".into(),
                    message: format!("Managed process `{name}` is no longer running"),
                });
            }

            process.stdin.clone()
        };
        timeout(CAPABILITY_TIMEOUT, async move {
            let mut stdin = stdin.lock().await;
            stdin.write_all(data.as_bytes()).await.map_err(host_error)?;
            stdin.flush().await.map_err(host_error)
        })
        .await
        .map_err(|_| timeout_error("process.send"))??;

        Ok(serde_json::json!({"message": serde_json::json!({"ok": true}).to_string()}))
    }

    /// Read data from a named process's stdout with optional framing.
    async fn handle_process_recv(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = string_argument(&request.arguments, "name")?;
        let key = self.managed_process_key(name, invoking_extension)?;
        let timeout_ms = bounded_positive_integer_argument(
            &request.arguments,
            "timeout_ms",
            DEFAULT_RECV_TIMEOUT_MS,
            MAX_RECV_TIMEOUT_MS,
        )?;
        let framing = request
            .arguments
            .get("framing")
            .and_then(Value::as_str)
            .unwrap_or("raw");

        let (stdout_buf, alive) = {
            let registry = self.managed_processes.lock().await;
            let process = registry.get(&key).ok_or_else(|| BrokerError {
                code: "not_found".into(),
                message: format!("No managed process named `{name}`"),
            })?;
            (process.stdout_buf.clone(), process.alive.clone())
        };

        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

        loop {
            {
                let mut buf = stdout_buf.lock().await;
                match framing {
                    "content-length" => {
                        if let Some((message, consumed)) = extract_content_length_message(&buf) {
                            buf.drain(..consumed);
                            return Ok(serde_json::json!({"message": serde_json::json!({
                                "data": message, "eof": false
                            }).to_string()}));
                        }
                    }
                    "line" => {
                        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&line_bytes).to_string();
                            return Ok(serde_json::json!({"message": serde_json::json!({
                                "data": line, "eof": false
                            }).to_string()}));
                        }
                    }
                    _ => {
                        // "raw" — return whatever is available
                        if !buf.is_empty() {
                            let data = std::mem::take(&mut *buf);
                            let text = String::from_utf8_lossy(&data).to_string();
                            return Ok(serde_json::json!({"message": serde_json::json!({
                                "data": text, "eof": false
                            }).to_string()}));
                        }
                    }
                }

                // Check if process is dead and buffer is drained.
                if !alive.load(Ordering::Relaxed) && buf.is_empty() {
                    return Ok(serde_json::json!({"message": serde_json::json!({
                        "data": "", "eof": true
                    }).to_string()}));
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Ok(serde_json::json!({"message": serde_json::json!({
                    "data": "", "eof": !alive.load(Ordering::Relaxed)
                }).to_string()}));
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Terminate a named process and remove it from the registry.
    async fn handle_process_kill(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = string_argument(&request.arguments, "name")?;
        let key = self.managed_process_key(name, invoking_extension)?;
        let mut registry = self.managed_processes.lock().await;
        let removed = registry.remove(&key);
        let Some(process) = removed else {
            return Err(BrokerError {
                code: "not_found".into(),
                message: format!("No managed process named `{name}`"),
            });
        };
        drop(registry);
        process
            .child
            .lock()
            .await
            .kill()
            .await
            .map_err(host_error)?;
        Ok(serde_json::json!({"message": serde_json::json!({"ok": true}).to_string()}))
    }

    /// List active managed processes or query a specific one.
    async fn handle_process_status(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = request.arguments.get("name").and_then(Value::as_str);
        let registry = self.managed_processes.lock().await;

        if let Some(name) = name {
            let key = self.managed_process_key(name, invoking_extension)?;
            if let Some(process) = registry.get(&key) {
                return Ok(serde_json::json!({"message": serde_json::json!({
                    "processes": [{
                        "name": name,
                        "pid": process.pid,
                        "alive": process.alive.load(Ordering::Relaxed),
                    }]
                }).to_string()}));
            }
            return Ok(serde_json::json!({"message": serde_json::json!({
                "processes": []
            }).to_string()}));
        }

        let processes: Vec<Value> = registry
            .iter()
            .filter(|(key, _)| {
                key.extension == invoking_extension
                    && key.session == self.extensions.active_session_scope().ok().flatten()
            })
            .map(|(key, p)| {
                serde_json::json!({
                    "name": key.name,
                    "pid": p.pid,
                    "alive": p.alive.load(Ordering::Relaxed),
                })
            })
            .collect();

        Ok(serde_json::json!({"message": serde_json::json!({
            "processes": processes
        }).to_string()}))
    }

    async fn handle_network_async(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        if request.operation != "http" {
            return unknown_operation(self.capability, &request.operation);
        }
        let url = string_argument(&request.arguments, "url")?;
        let method = string_argument(&request.arguments, "method")?;
        let body = request
            .arguments
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_argument("missing argument `body`"))?;
        let (host, port, path) = parse_http_url(url)?;
        if !self.allowed_hosts.contains(host) {
            return Err(BrokerError {
                code: "host_denied".into(),
                message: format!("Network host `{host}` is not allowed"),
            });
        }
        let host = host.to_string();
        let request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let result = timeout(CAPABILITY_TIMEOUT, async move {
            let mut stream = tokio::net::TcpStream::connect((host.as_str(), port))
                .await
                .map_err(host_error)?;
            tokio::io::AsyncWriteExt::write_all(&mut stream, request.as_bytes())
                .await
                .map_err(host_error)?;
            let response = read_limited(
                &mut stream,
                "network_response_too_large",
                "network response",
                MAX_CAPABILITY_BUFFER_BYTES,
            )
            .await?;
            String::from_utf8(response).map_err(|_| invalid_argument("response was not UTF-8"))
        })
        .await
        .map_err(|_| timeout_error("network.http"))??;
        Ok(serde_json::json!({"message": result}))
    }

    fn handle_ui(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let message = match request.operation.as_str() {
            "notify" => string_argument(&request.arguments, "message")?,
            "set_status" => string_argument(&request.arguments, "status")?,
            _ => return unknown_operation(self.capability, &request.operation),
        };
        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
            text_delta: Some(message.to_string()),
            reasoning_delta: None,
            tool_call_name: None,
        });
        Ok(Value::Null)
    }

    fn handle_events(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let topic = string_argument(&request.arguments, "topic")?;
        match request.operation.as_str() {
            "subscribe" => {
                if invoking_extension.is_empty() {
                    return Err(invalid_argument(
                        "events subscription requires host extension identity",
                    ));
                }
                self.extensions
                    .subscribe_event(invoking_extension, topic.to_string())
                    .map_err(host_error)?;
                Ok(Value::Null)
            }
            "publish" => {
                let payload = request
                    .arguments
                    .get("payload")
                    .cloned()
                    .ok_or_else(|| invalid_argument("missing argument `payload`"))?;
                self.extensions
                    .publish_event(topic.to_string(), payload)
                    .map_err(host_error)?;
                Ok(Value::Null)
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }
}

async fn read_limited(
    mut reader: impl AsyncRead + Unpin,
    code: &'static str,
    source: &'static str,
    max_bytes: usize,
) -> Result<Vec<u8>, BrokerError> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let read = reader.read(&mut chunk).await.map_err(host_error)?;
        if read == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(read) > max_bytes {
            return Err(BrokerError {
                code: code.into(),
                message: format!("{source} exceeds the {max_bytes}-byte buffer limit"),
            });
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessRunLimits {
    timeout: Duration,
    max_output_bytes: usize,
}

fn process_run_limits(arguments: &Value) -> Result<ProcessRunLimits, BrokerError> {
    let timeout_ms = bounded_positive_integer_argument(
        arguments,
        "timeout_ms",
        CAPABILITY_TIMEOUT.as_millis() as u64,
        MAX_PROCESS_TIMEOUT_MS,
    )?;
    let max_output_bytes = bounded_positive_integer_argument(
        arguments,
        "max_output_bytes",
        MAX_CAPABILITY_BUFFER_BYTES as u64,
        MAX_PROCESS_OUTPUT_BYTES as u64,
    )? as usize;
    Ok(ProcessRunLimits {
        timeout: Duration::from_millis(timeout_ms),
        max_output_bytes,
    })
}

fn bounded_positive_integer_argument(
    arguments: &Value,
    name: &str,
    default: u64,
    max: u64,
) -> Result<u64, BrokerError> {
    let Some(value) = arguments.get(name) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_argument(format!("`{name}` must be a positive integer")))?;
    Ok(value.min(max))
}

fn timeout_error(operation: &str) -> BrokerError {
    BrokerError {
        code: "timeout".into(),
        message: format!("Capability operation `{operation}` timed out"),
    }
}
fn internal_error(message: impl Into<String>) -> BrokerError {
    BrokerError {
        code: "host_error".into(),
        message: message.into(),
    }
}
fn host_error(error: impl std::fmt::Display) -> BrokerError {
    internal_error(error.to_string())
}
fn invalid_argument(message: impl Into<String>) -> BrokerError {
    BrokerError {
        code: "invalid_argument".into(),
        message: message.into(),
    }
}
fn unknown_operation(capability: &str, operation: &str) -> Result<Value, BrokerError> {
    Err(BrokerError {
        code: "unknown_operation".into(),
        message: format!("Capability `{capability}` does not implement operation `{operation}`"),
    })
}
fn string_argument<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, BrokerError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid_argument(format!("missing or empty argument `{name}`")))
}

fn extract_content_length_message(buffer: &[u8]) -> Option<(String, usize)> {
    let header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let header = std::str::from_utf8(&buffer[..header_end]).ok()?;
    let content_length = header.lines().find_map(|line| {
        line.split_once(':').and_then(|(name, value)| {
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
    })?;
    let message_end = header_end.checked_add(content_length)?;
    let body = buffer.get(header_end..message_end)?;
    Some((String::from_utf8(body.to_vec()).ok()?, message_end))
}
fn resolve_work_path(work_dir: &Path, relative: &str) -> Result<PathBuf, BrokerError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(invalid_argument("path must remain under work_dir"));
    }
    let root = work_dir.canonicalize().map_err(host_error)?;
    let candidate = root.join(path);
    let checked = if candidate.exists() {
        candidate.canonicalize().map_err(host_error)?
    } else {
        candidate
            .parent()
            .ok_or_else(|| invalid_argument("invalid path"))?
            .canonicalize()
            .map_err(host_error)?
            .join(
                candidate
                    .file_name()
                    .ok_or_else(|| invalid_argument("invalid path"))?,
            )
    };
    if !checked.starts_with(&root) {
        return Err(invalid_argument("path escapes work_dir"));
    }
    Ok(checked)
}
fn parse_http_url(url: &str) -> Result<(&str, u16, String), BrokerError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| invalid_argument("only http:// URLs are supported"))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = authority
        .rsplit_once(':')
        .map_or((authority, 80), |(host, port)| {
            (host, port.parse().unwrap_or(0))
        });
    if host.is_empty() || port == 0 {
        return Err(invalid_argument("invalid URL"));
    }
    Ok((host, port, format!("/{path}")))
}

#[derive(Clone)]
pub struct CodingAgentOptions {
    pub api_key: String,
    pub account_id: Option<String>,
    pub model: String,
    pub work_dir: PathBuf,
    pub session_file: Option<PathBuf>,
    pub system_prompt: SystemPromptConfig,
}

pub struct ExtensionBeforeToolHook {
    pub tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub extensions: Arc<WasiExtensionManager>,
    pub broker_dispatcher: Arc<CapabilityDispatcher>,
}

#[async_trait]
impl BeforeToolCallHook for ExtensionBeforeToolHook {
    async fn before_tool_call(
        &self,
        tool_call: &AgentToolCall,
        _state: &AgentState,
    ) -> BeforeToolCallResult {
        let policy = *self.tool_policy.lock().await;
        if policy == ToolPolicy::ReadOnly
            && matches!(
                tool_call.name.as_str(),
                "write_file" | "edit_file" | "write" | "edit" | "run_command"
            )
        {
            return BeforeToolCallResult {
                block: true,
                reason: Some(format!(
                    "Tool `{}` is blocked because read-only tool policy is ACTIVE.",
                    tool_call.name
                )),
            };
        }

        let arguments = serde_json::json!({
            "tool_name": tool_call.name,
            "tool_arguments": tool_call.arguments,
        });
        let hook_responses = self
            .extensions
            .execute_hook_with_broker_requests("before_tool_call", &arguments.to_string());
        for resp in hook_responses {
            let res = match resp {
                Ok(res) => res,
                Err(error) => {
                    return BeforeToolCallResult {
                        block: true,
                        reason: Some(format!("Extension hook error: {error}")),
                    };
                }
            };
            if let Err(error) = dispatch_hook_requests(
                &self.broker_dispatcher,
                &self.extensions,
                res.host_broker_requests,
            )
            .await
            {
                return BeforeToolCallResult {
                    block: true,
                    reason: Some(format!("Extension broker error: {}", error.message)),
                };
            }
            let api_version = res.api_version;
            let response = res.response;
            if api_version == BROKER_API_VERSION {
                if let Some(middleware) = response.middleware {
                    if middleware.block == Some(true) {
                        return BeforeToolCallResult {
                            block: true,
                            reason: middleware.reason,
                        };
                    }
                }
            } else if api_version == 1 {
                if let Some(msg) = response.message {
                    if msg.contains("blocked") {
                        return BeforeToolCallResult {
                            block: true,
                            reason: Some(msg),
                        };
                    }
                }
            }
        }

        BeforeToolCallResult::default()
    }
}

pub struct ExtensionAfterToolHook {
    extensions: Arc<WasiExtensionManager>,
    broker_dispatcher: Arc<CapabilityDispatcher>,
}

#[async_trait]
impl AfterToolCallHook for ExtensionAfterToolHook {
    async fn after_tool_call(
        &self,
        tool_call: &AgentToolCall,
        result: &AgentToolResult,
        _state: &AgentState,
    ) -> AfterToolCallResult {
        let arguments = serde_json::json!({
            "tool_name": tool_call.name,
            "tool_arguments": tool_call.arguments,
            "result": result.content,
            "is_error": result.is_error,
        });
        // Tool requests are queued by ToolExecutor; dispatch them first so the
        // tool's effects precede the deterministic, name-sorted after hooks.
        dispatch_hook_requests_isolated(
            &self.broker_dispatcher,
            &self.extensions,
            self.extensions.take_pending_broker_requests(),
            "WASI tool broker error",
        )
        .await;
        for response in self
            .extensions
            .execute_hook_with_broker_requests("after_tool_call", &arguments.to_string())
        {
            match response {
                Ok(response) => {
                    match self
                        .broker_dispatcher
                        .dispatch_envelopes(response.host_broker_requests)
                        .await
                    {
                        Ok(dispatch) => {
                            self.extensions
                                .enqueue_broker_results(dispatch.operation_results);
                        }
                        Err(error) => {
                            eprintln!("WASI after-tool hook broker error: {}", error.message)
                        }
                    }
                }
                Err(error) => eprintln!("WASI after-tool hook error: {error}"),
            }
        }
        AfterToolCallResult::default()
    }
}

struct BrokerAwareWasiToolExecutor {
    extensions: Arc<WasiExtensionManager>,
    broker_dispatcher: Arc<CapabilityDispatcher>,
}

#[async_trait]
impl ToolExecutor for BrokerAwareWasiToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.wasi_broker_tools"
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        self.extensions.tool_definitions()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        let mut continuation_rounds = 0;
        loop {
            let invocation = match self
                .extensions
                .execute_tool_with_broker_requests(name, args)?
            {
                Ok(invocation) => invocation,
                Err(error) => return Some(Err(error)),
            };
            if let Some(error) = invocation.response.error {
                return Some(Err(error));
            }
            let continue_after_broker = invocation.response.continue_after_broker;
            let immediate_message = invocation.response.message.unwrap_or_default();
            let requests = invocation.host_broker_requests;
            if requests.is_empty() {
                return Some(Ok(immediate_message));
            }
            if continue_after_broker && continuation_rounds >= MAX_BROKER_CONTINUATION_ROUNDS {
                return Some(Err(format!(
                    "WASI tool `{name}` exceeded the broker continuation limit of \
                     {MAX_BROKER_CONTINUATION_ROUNDS} rounds; clear `continue_after_broker` after \
                     processing `broker_response` events"
                )));
            }

            let dispatch = match self.broker_dispatcher.dispatch_envelopes(requests).await {
                Ok(dispatch) => dispatch,
                Err(error) => return Some(Err(error.message)),
            };
            let operation_results = dispatch.operation_results;
            self.extensions
                .enqueue_broker_results(operation_results.clone());

            if continue_after_broker {
                continuation_rounds += 1;
                continue;
            }

            if let Some(error) = operation_results
                .iter()
                .find_map(|result| result.error.as_ref())
            {
                return Some(Err(error.message.clone()));
            }

            let broker_message = operation_results
                .iter()
                .find(|result| {
                    result.request.capability == "agent" && result.request.operation == "run"
                })
                .or_else(|| operation_results.last())
                .and_then(|result| {
                    result
                        .value
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| result.value.get("output").and_then(Value::as_str))
                        .map(str::to_owned)
                });
            return Some(Ok(broker_message.unwrap_or(immediate_message)));
        }
    }
}

pub struct CodingAgent {
    agent: Agent,
    pub session_tree: SessionTree,
    pub wasi_extensions: Arc<WasiExtensionManager>,
    tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    work_dir: PathBuf,
    pub skills: Arc<SkillRegistry>,
    agent_runner: AgentRunner,
    broker_dispatcher: Arc<CapabilityDispatcher>,
    managed_processes: ManagedProcessRegistry,
    agent_work: AgentWorkScheduler,
    mcp_manager: Arc<McpManager>,
    plan_store: SessionPlanStore,
    prompt_templates: Option<Vec<crate::prompt_templates::PromptTemplate>>,
    dispatch_parent_leaf: Arc<std::sync::Mutex<Option<String>>>,
    completed_subagent_lanes: Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
    harness_journal: Option<HarnessJournal>,
    harness_journal_error: Option<String>,
    harness_run_id: Arc<std::sync::Mutex<Option<String>>>,
    cancellation: CodingAgentCancellation,
    interrupted_subagent_recovery: InterruptedSubagentRecoveryState,
    #[cfg(test)]
    subagent_work_observer: SubagentObserverState,
    #[cfg(test)]
    subagent_branch_observer: Option<SubagentBoundaryObserver>,
}

struct HarnessJournal {
    store: AgentHarness<JsonlStore>,
    cancellation: Arc<AtomicBool>,
}

pub struct HarnessWatch {
    hub: HarnessEventHub,
    subscription: Subscription,
}

impl HarnessWatch {
    pub fn snapshot(&self) -> &Snapshot {
        &self.subscription.snapshot
    }

    pub fn poll(&mut self) -> Result<Vec<HarnessEvent>, EventError> {
        self.hub.poll(&mut self.subscription)
    }
}

fn harness_event_hub(path: &Path) -> HarnessEventHub {
    static HUBS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, HarnessEventHub>>> =
        std::sync::OnceLock::new();
    let hubs = HUBS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut hubs = hubs.lock().unwrap_or_else(|error| error.into_inner());
    hubs.entry(path.to_path_buf())
        .or_insert_with(|| HarnessEventHub::new(256))
        .clone()
}

fn harness_cancellation_state(path: &Path) -> Arc<AtomicBool> {
    static STATES: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<AtomicBool>>>> =
        std::sync::OnceLock::new();
    let states = STATES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut states = states.lock().unwrap_or_else(|error| error.into_inner());
    states
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

impl HarnessJournal {
    fn open(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| error.to_string())?;
        }
        let events = harness_event_hub(path);
        let persist_path = path.to_path_buf();
        let persist_events = events.clone();
        let executor = move |action: threadlane_agent::harness::EffectAction| {
            let mut store = JsonlStore::open(&persist_path).map_err(|error| {
                threadlane_agent::harness::ReduceError::Storage(error.to_string())
            })?;
            if let Err(error) = action.apply(&mut store) {
                persist_events.publish(threadlane_agent::harness::EventPayload::Fault(
                    error.to_string(),
                ));
                return Err(error);
            }
            let (payload, lane, run_id, turn) = match &action {
                threadlane_agent::harness::EffectAction::AppendEntry { entry } => (
                    threadlane_agent::harness::EventPayload::EntryCommitted(entry.clone()),
                    Some(entry.lane.clone()),
                    None,
                    None,
                ),
                threadlane_agent::harness::EffectAction::AppendRecord { record, .. } => (
                    threadlane_agent::harness::EventPayload::RecordCommitted(record.clone()),
                    Some(record.lane().to_owned()),
                    record.run_id().map(str::to_owned),
                    record.turn(),
                ),
            };
            persist_events.publish_identified_with_turn(payload, lane, run_id, turn, None);
            Ok(())
        };
        let cancellation = harness_cancellation_state(path);
        JsonlStore::open(path)
            .map(|store| Self {
                store: AgentHarness::with_executor(store, events, executor),
                cancellation,
            })
            .map_err(|error| error.to_string())
    }

    fn append_message_to_path(path: &Path, message: AgentMessage) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.append_message(message)
    }

    fn append_tool_intent_to_path(
        path: &Path,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.append_tool_intent(run_id, tool_call_id, tool_name, effective_args)
    }

    #[cfg(test)]
    fn start(&mut self, run_id: &str, source_leaf_id: Option<String>) -> Result<(), String> {
        self.refresh()?;
        self.store
            .start_operation(run_id, source_leaf_id, OperationIntent::Run)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn start_with_prompt(&mut self, run_id: &str, prompt: AgentMessage) -> Result<(), String> {
        self.refresh()?;
        self.store
            .accept_prompt(run_id, prompt)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn run_before_tool_hook(
        &self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
    ) -> Result<(), String> {
        let context = HookContext {
            session_id: self.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.into()),
            resume_data: None,
        };
        self.store
            .hooks()
            .run_before_tool(&context)
            .map_err(|failures| {
                failures
                    .into_iter()
                    .map(|failure| {
                        format!(
                            "{} ({tool_call_id}/{tool_name}): {}",
                            failure.id, failure.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
    }

    fn run_after_tool_hook(&self, run_id: &str) {
        let context = HookContext {
            session_id: self.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.into()),
            resume_data: None,
        };
        for failure in self.store.hooks().run(HookKind::AfterTool, &context) {
            eprintln!("after-tool hook {} failed: {}", failure.id, failure.message);
        }
    }

    fn unique_run_id(&mut self, prefix: &str) -> Result<String, String> {
        self.refresh()?;
        let used_ids = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.id.clone())
            .chain(
                self.store
                    .records()
                    .iter()
                    .map(|record| record.id().to_owned()),
            )
            .collect::<Vec<_>>();
        Ok(SessionIdGenerator::new(self.store.session_id()).next(prefix, &used_ids))
    }

    fn finish(
        &mut self,
        run_id: &str,
        outcome: OpOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.refresh()?;
        let outcome = match outcome {
            OpOutcome::Completed => OperationOutcome::Completed,
            OpOutcome::Aborted => OperationOutcome::Aborted,
            OpOutcome::Failed => OperationOutcome::Failed,
            OpOutcome::Declined => OperationOutcome::Declined,
        };
        self.store
            .finish_operation(run_id, outcome, error)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn schedule_retry(&mut self, run_id: &str, reason: &str) -> Result<u32, String> {
        self.refresh()?;
        let attempt = self
            .store
            .schedule_retry(
                run_id,
                reason,
                RetryPolicy {
                    max_attempts: 3,
                    base_delay: 1_000,
                    max_delay: 8_000,
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(attempt)
    }

    fn begin_retry(&mut self, run_id: &str) -> Result<u32, String> {
        self.refresh()?;
        let attempt = self
            .store
            .begin_retry(run_id)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(attempt)
    }

    fn append_message(&mut self, message: AgentMessage) -> Result<(), String> {
        self.refresh()?;
        if self
            .store
            .entries()
            .last()
            .is_some_and(|entry| entry.message == message)
        {
            return Ok(());
        }
        let parent_id = Reducer::reduce(&self.store)
            .ok()
            .and_then(|state| state.lane("main").and_then(|lane| lane.leaf_id.clone()))
            .or_else(|| {
                self.store
                    .entries()
                    .iter()
                    .rev()
                    .find(|entry| entry.lane == "main")
                    .map(|entry| entry.id.clone())
            });
        let seq = self.next_seq();
        let terminate = matches!(
            &message,
            AgentMessage::Tool {
                terminate: true,
                ..
            }
        );
        let id = match &message {
            AgentMessage::Assistant { .. } => Reducer::reduce(&self.store)
                .ok()
                .and_then(|state| {
                    state
                        .lane("main")
                        .and_then(|lane| lane.open_operation.clone())
                })
                .and_then(|run_id| {
                    self.store
                        .records()
                        .iter()
                        .rev()
                        .find_map(|record| match record {
                            HarnessRecord::StepAttempt {
                                run_id: record_run_id,
                                result_entry_id,
                                ..
                            } if record_run_id == &run_id
                                && !self
                                    .store
                                    .entries()
                                    .iter()
                                    .any(|entry| entry.id == result_entry_id.as_str()) =>
                            {
                                Some(result_entry_id.clone())
                            }
                            _ => None,
                        })
                })
                .unwrap_or_else(|| format!("v2-entry-{seq}")),
            AgentMessage::Tool { tool_call_id, .. } => format!("v2-tool-result-{tool_call_id}"),
            _ => format!("v2-entry-{seq}"),
        };
        self.store
            .append_entry_gated(HarnessEntry {
                id,
                parent_id,
                lane: "main".into(),
                seq,
                timestamp: timestamp(),
                message,
                terminate,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn prepare_assistant_attempt(&mut self, run_id: &str) -> Result<String, String> {
        self.refresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let lane = state
            .lane("main")
            .filter(|lane| lane.open_operation.as_deref() == Some(run_id))
            .ok_or_else(|| format!("harness operation {run_id} is not open"))?;

        if let Some(result_entry_id) = self.store.records().iter().find_map(|record| {
            let HarnessRecord::StepAttempt {
                run_id: record_run_id,
                result_entry_id,
                ..
            } = record
            else {
                return None;
            };
            (record_run_id == run_id
                && !self
                    .store
                    .entries()
                    .iter()
                    .any(|entry| entry.id == *result_entry_id))
            .then(|| result_entry_id.clone())
        }) {
            return Ok(result_entry_id);
        }

        let attempt = lane.attempts.saturating_add(1);
        let result_entry_id = format!("entry-{run_id}-assistant-{attempt}");
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::StepAttempt {
                id: format!("attempt-{run_id}-{attempt}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                result_entry_id: result_entry_id.clone(),
                compaction_reason: None,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(result_entry_id)
    }

    fn finish_tool_message(&mut self, run_id: &str, message: &AgentMessage) -> Result<(), String> {
        let AgentMessage::Tool {
            tool_call_id,
            name,
            content,
            is_error,
            terminate,
        } = message
        else {
            return Ok(());
        };
        self.refresh()?;
        self.store
            .finish_existing_tool(
                run_id,
                threadlane_agent::harness::ToolResult {
                    call_id: tool_call_id.clone(),
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    terminate: *terminate,
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn finish_replayed_tool(
        &mut self,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .finish_existing_tool(
                run_id,
                HarnessToolResult {
                    call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    terminate: result.terminates(),
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn append_replayed_tool_entry(
        &mut self,
        run_id: &str,
        assistant_entry_id: &str,
        spec: &ToolSpec,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        let state = Reducer::reduce(self.store.store()).map_err(|error| error.to_string())?;
        let lane = state
            .lanes
            .iter()
            .find(|lane| lane.open_operation.as_deref() == Some(run_id))
            .ok_or_else(|| format!("harness operation {run_id} is not open"))?;
        let seq = self.next_seq();
        self.store
            .append_entry_gated(HarnessEntry {
                id: spec.result_entry_id.clone(),
                parent_id: Some(assistant_entry_id.into()),
                lane: lane.name.clone(),
                seq,
                timestamp: timestamp(),
                message: AgentMessage::Tool {
                    tool_call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    terminate: result.terminates(),
                },
                terminate: result.terminates(),
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn append_tool_intent(
        &mut self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.refresh()?;
        // A persisted ToolStarted is already past preparation. Do not rerun
        // the fail-closed before-tool hook on recovery or duplicate delivery.
        if self.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ToolStarted {
                run_id: record_run_id,
                tool_call_id: record_call_id,
                ..
            } if record_run_id == run_id && record_call_id == tool_call_id)
        }) {
            return Ok(());
        }
        self.run_before_tool_hook(run_id, tool_call_id, tool_name)?;
        let assistant = self
            .store
            .entries()
            .iter()
            .rev()
            .find(|entry| {
                matches!(
                    &entry.message,
                    AgentMessage::Assistant { tool_calls: Some(calls), .. }
                        if calls.iter().any(|call| call.id == tool_call_id)
                )
            })
            .ok_or_else(|| format!("missing assistant entry for tool {tool_call_id}"))?;
        let assistant_id = assistant.id.clone();
        let tool_index = match &assistant.message {
            AgentMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => calls
                .iter()
                .position(|call| call.id == tool_call_id)
                .ok_or_else(|| format!("tool {tool_call_id} is absent from assistant entry"))?,
            _ => return Err("assistant entry has no tool calls".into()),
        };
        self.store
            .start_tool_batch(
                run_id,
                &assistant_id,
                &[ToolSpec {
                    index: tool_index,
                    call_id: tool_call_id.into(),
                    name: tool_name.into(),
                    effective_args,
                    result_entry_id: format!("v2-tool-result-{tool_call_id}"),
                    replay: match threadlane_agent::classify_tool_replay_safety(tool_name) {
                        threadlane_agent::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                        threadlane_agent::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
                    },
                }],
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn record_assistant_attempt(&mut self, run_id: &str, usage: TokenUsage) -> Result<(), String> {
        self.refresh()?;
        let start_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        let result_entry_id = self
            .store
            .entries()
            .iter()
            .filter(|entry| {
                entry.seq > start_seq
                    && matches!(
                        &entry.message,
                        threadlane_agent::AgentMessage::Assistant { .. }
                    )
            })
            .max_by_key(|entry| entry.seq)
            .map(|entry| entry.id.clone())
            .ok_or_else(|| format!("run {run_id} has no assistant result"))?;
        self.store
            .finish_assistant_attempt(run_id, &result_entry_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn record_provider_usage(&mut self, run_id: &str, usage: TokenUsage) -> Result<(), String> {
        self.refresh()?;
        self.store
            .record_provider_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn record_discarded_usage(&mut self, run_id: &str, usage: TokenUsage) -> Result<(), String> {
        self.refresh()?;
        self.store
            .record_discarded_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn record_completed_tools_with_termination(
        &mut self,
        run_id: &str,
        termination: &HashMap<String, bool>,
    ) -> Result<(), String> {
        self.refresh()?;
        let start_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        let Some(assistant) = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.seq > start_seq)
            .filter_map(|entry| match &entry.message {
                AgentMessage::Assistant {
                    tool_calls: Some(tool_calls),
                    ..
                } if !tool_calls.is_empty() => Some(entry),
                _ => None,
            })
            .max_by_key(|entry| entry.seq)
        else {
            return Ok(());
        };
        let assistant_id = assistant.id.clone();
        let tool_entries = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.parent_id.as_deref() == Some(assistant_id.as_str()))
            .filter_map(|entry| match &entry.message {
                AgentMessage::Tool {
                    tool_call_id, name, ..
                } => Some((tool_call_id.clone(), name.clone(), entry.id.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let AgentMessage::Assistant {
            tool_calls: Some(tool_calls),
            ..
        } = &assistant.message
        else {
            return Ok(());
        };
        let tool_calls = tool_calls.clone();
        let mut pending_results = Vec::new();
        if tool_calls
            .iter()
            .any(|call| !tool_entries.iter().any(|(id, _, _)| id == &call.id))
        {
            return Err(format!("run {run_id} has an incomplete tool batch"));
        }
        for (index, call) in tool_calls.iter().enumerate() {
            let (_, name, result_entry) = tool_entries
                .iter()
                .find(|(id, _, _)| id == &call.id)
                .expect("tool batch completeness was checked");
            let persisted_termination = self
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == *result_entry)
                .is_some_and(|entry| entry.terminate);
            let persisted_result = self
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == *result_entry)
                .and_then(|entry| match &entry.message {
                    AgentMessage::Tool {
                        content, is_error, ..
                    } => Some((content.clone(), *is_error)),
                    _ => None,
                })
                .ok_or_else(|| format!("run {run_id} has an invalid tool result"))?;
            let args = serde_json::from_str(&call.function.arguments)
                .unwrap_or_else(|_| serde_json::Value::String(call.function.arguments.clone()));
            let replay = match threadlane_agent::classify_tool_replay_safety(name) {
                threadlane_agent::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                threadlane_agent::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
            };
            let started = self.store.records().iter().any(|record| {
                matches!(record, HarnessRecord::ToolStarted {
                    run_id: record_run_id,
                    tool_call_id,
                    ..
                } if record_run_id == run_id && tool_call_id == &call.id)
            });
            if !started {
                self.store
                    .start_tool_batch(
                        run_id,
                        &assistant_id,
                        &[ToolSpec {
                            index,
                            call_id: call.id.clone(),
                            name: name.to_string(),
                            effective_args: args,
                            result_entry_id: result_entry.clone(),
                            replay,
                        }],
                    )
                    .map_err(|error| error.to_string())?;
                self.store
                    .drive_to_completion()
                    .map_err(|error| error.to_string())?;
            }
            let finished = self.store.records().iter().any(|record| {
                matches!(record, HarnessRecord::ToolFinished {
                    run_id: record_run_id,
                    tool_call_id,
                    ..
                } if record_run_id == run_id && tool_call_id == &call.id)
            });
            if finished {
                continue;
            }
            pending_results.push(threadlane_agent::harness::ToolResult {
                call_id: call.id.clone(),
                name: name.to_string(),
                content: persisted_result.0,
                is_error: persisted_result.1,
                terminate: termination
                    .get(&call.id)
                    .copied()
                    .unwrap_or(persisted_termination),
            });
        }
        if !pending_results.is_empty() {
            self.store
                .finish_existing_tool_batch(run_id, &pending_results, TokenUsage::default())
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn request_abort(&mut self) -> Result<Option<String>, String> {
        self.cancellation.store(true, Ordering::SeqCst);
        self.refresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let open_lanes: Vec<(String, String)> = state
            .lanes
            .iter()
            .filter_map(|lane| {
                lane.open_operation
                    .as_ref()
                    .map(|run_id| (lane.name.clone(), run_id.clone()))
            })
            .collect();
        if open_lanes.is_empty() {
            return Ok(None);
        }
        let main_run_id = state
            .lane("main")
            .and_then(|lane| lane.open_operation.clone());
        for (lane_name, run_id) in open_lanes {
            let is_already_requested = state.lane(&lane_name).is_some_and(|l| l.abort_requested);
            if !is_already_requested {
                let _ = self.store.request_abort(&run_id);
                let _ = self.store.drive_to_completion();
            }
        }
        Ok(main_run_id)
    }

    fn recover_abort(&mut self) -> Result<bool, String> {
        self.refresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let Some(lane) = state.lane("main") else {
            return Ok(false);
        };
        let Some(run_id) = lane.open_operation.clone() else {
            return Ok(false);
        };
        if !lane.abort_requested {
            return Err(format!("suspended harness operation {run_id}"));
        }
        let start_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == &run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        if let Some(assistant_entry_id) = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.seq > start_seq)
            .find_map(|entry| {
                matches!(&entry.message, AgentMessage::Assistant { .. }).then_some(entry.id.clone())
            })
        {
            self.store
                .reconcile_abort(&run_id, &assistant_entry_id)
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            return Ok(true);
        }
        let result_entry_id = self.store.records().iter().rev().find_map(|record| {
            matches!(record, HarnessRecord::StepAttempt { run_id: record_run_id, .. } if record_run_id == &run_id)
                .then(|| match record {
                    HarnessRecord::StepAttempt { result_entry_id, .. } => result_entry_id.clone(),
                    _ => unreachable!(),
                })
        });
        let had_result_entry = result_entry_id.is_some();
        let entry_id = result_entry_id.unwrap_or_else(|| format!("abort-entry-{run_id}"));
        let has_abort_entry = self.store.entries().iter().any(|entry| {
            entry.id == entry_id
                && matches!(
                    &entry.message,
                    threadlane_agent::AgentMessage::Assistant {
                        stop_reason: Some(reason),
                        ..
                    } if reason == "aborted"
                )
        });
        if !had_result_entry && !has_abort_entry {
            self.store
                .append_record_gated(HarnessRecord::StepAttempt {
                    id: format!("abort-attempt-{run_id}"),
                    seq: self.next_seq(),
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.clone(),
                    attempt: lane.attempts.saturating_add(1),
                    result_entry_id: entry_id.clone(),
                    compaction_reason: None,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        if !has_abort_entry {
            let seq = self.next_seq();
            self.store
                .append_entry_gated(threadlane_agent::harness::Entry {
                    id: entry_id.clone(),
                    parent_id: lane.leaf_id.clone(),
                    lane: "main".into(),
                    seq,
                    timestamp: timestamp(),
                    message: threadlane_agent::AgentMessage::Assistant {
                        content: Some("Run aborted before completion.".into()),
                        tool_calls: None,
                        stop_reason: Some("aborted".into()),
                        deferred_handle: None,
                    },
                    terminate: false,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        self.finish(
            &run_id,
            OpOutcome::Aborted,
            Some("Generation cancelled".into()),
        )?;
        Ok(true)
    }

    fn redeem_deferred(
        &mut self,
        run_id: &str,
        resolution: DeferredResolution,
    ) -> Result<bool, String> {
        self.refresh()?;
        let terminal = self
            .store
            .redeem_deferred(run_id, resolution)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        if terminal {
            self.finish(run_id, OpOutcome::Completed, None)?;
        }
        Ok(terminal)
    }

    fn next_seq(&self) -> u64 {
        self.store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.store.records().iter().map(HarnessRecord::seq))
            .max()
            .unwrap_or(0)
            + 1
    }

    fn refresh(&mut self) -> Result<(), String> {
        let path = self.store.store().path().to_path_buf();
        let hooks = std::mem::take(self.store.hooks_mut());
        match Self::open(&path) {
            Ok(mut refreshed) => {
                *refreshed.store.hooks_mut() = hooks;
                self.store = refreshed.store;
                Ok(())
            }
            Err(error) => {
                *self.store.hooks_mut() = hooks;
                Err(error)
            }
        }
    }

    fn start_subagent_lane(
        &mut self,
        lane_hint: &str,
        task: &str,
        source_leaf_id: Option<&str>,
    ) -> Result<SubagentLaneIdentity, SubagentStartError> {
        if self.cancellation.load(Ordering::SeqCst) {
            return Err(SubagentStartError {
                identity: None,
                error: "Subagent start rejected because the parent is cancelling".into(),
            });
        }
        static START_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _start_lock = START_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .map_err(|error| SubagentStartError {
                identity: None,
                error: error.to_string(),
            })?;
        let mut attempt_idx = 0;
        let identity = loop {
            self.refresh().map_err(|error| SubagentStartError {
                identity: None,
                error: error.to_string(),
            })?;
            let used_ids = self
                .store
                .entries()
                .iter()
                .map(|entry| entry.id.clone())
                .chain(
                    self.store
                        .records()
                        .iter()
                        .flat_map(|record| [record.id().to_owned(), record.lane().to_owned()]),
                )
                .collect::<Vec<_>>();
            let generator = SessionIdGenerator::new(self.store.session_id());
            let base_run_id = generator.next("subagent-run", &used_ids);
            let run_id = if attempt_idx == 0 {
                base_run_id
            } else {
                format!("{base_run_id}-{attempt_idx}")
            };
            let mut lane_ids = used_ids.clone();
            lane_ids.push(run_id.clone());
            let base_lane = generator.next(lane_hint, &lane_ids);
            let lane_name = if attempt_idx == 0 {
                base_lane
            } else {
                format!("{base_lane}-{attempt_idx}")
            };
            let identity = SubagentLaneIdentity {
                lane_name: lane_name.clone(),
                run_id: run_id.clone(),
                source_leaf_id: source_leaf_id.map(str::to_owned),
                started_seq: 0,
            };
            if let Err(error) = self.store.start_operation_on_lane(
                &lane_name,
                &run_id,
                source_leaf_id.map(str::to_owned),
                OperationIntent::Run,
            ) {
                let err_str = error.to_string();
                if err_str.contains("DuplicateId") {
                    attempt_idx += 1;
                    continue;
                }
                if source_leaf_id.is_some()
                    && (err_str.contains("source leaf does not exist")
                        || err_str.contains("MissingParent"))
                {
                    if let Err(retry_err) = self.store.start_operation_on_lane(
                        &lane_name,
                        &run_id,
                        None,
                        OperationIntent::Run,
                    ) {
                        if retry_err.to_string().contains("DuplicateId") {
                            attempt_idx += 1;
                            continue;
                        }
                        return Err(SubagentStartError {
                            identity: None,
                            error: retry_err.to_string(),
                        });
                    }
                } else {
                    return Err(SubagentStartError {
                        identity: None,
                        error: err_str,
                    });
                }
            }
            break identity;
        };
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        let prompt_message = AgentMessage::user(task.to_owned(), Vec::new());
        let prompt_entry_id = format!("subagent-entry-{}-0", identity.run_id);
        let effective_parent_id = source_leaf_id
            .filter(|id| self.store.entries().iter().any(|e| e.id == *id))
            .map(str::to_owned);
        self.store
            .append_entry_gated(threadlane_agent::harness::Entry {
                id: prompt_entry_id,
                parent_id: effective_parent_id,
                lane: identity.lane_name.clone(),
                seq: harness_next_seq(self.store.store()),
                timestamp: timestamp(),
                message: prompt_message,
                terminate: false,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .append_record_gated(HarnessRecord::StepAttempt {
                id: format!("assistant-attempt-action-{}-1", identity.run_id),
                seq: harness_next_seq(self.store.store()),
                lane: identity.lane_name.clone(),
                timestamp: timestamp(),
                run_id: identity.run_id.clone(),
                attempt: 1,
                result_entry_id: format!("entry-{}-assistant-1", identity.run_id),
                compaction_reason: None,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        Ok(identity)
    }

    fn finish_subagent_lane(
        &mut self,
        _lane: &str,
        run_id: &str,
        outcome: OpOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.refresh()?;
        let is_open = Reducer::reduce(self.store.store()).ok().map(|state| {
            state
                .lanes
                .iter()
                .any(|l| l.open_operation.as_deref() == Some(run_id))
        }) == Some(true);
        if !is_open {
            return Ok(());
        }

        if outcome == OpOutcome::Aborted {
            let mut any_provisioned = false;
            if let Ok(state) = Reducer::reduce(self.store.store()) {
                if let Some(l) = state
                    .lanes
                    .iter()
                    .find(|l| l.open_operation.as_deref() == Some(run_id))
                {
                    for tool in &l.tools {
                        if !tool.completed && tool.run_id == run_id {
                            if !self
                                .store
                                .entries()
                                .iter()
                                .any(|entry| entry.id == tool.result_entry_id)
                            {
                                self.append_message_to_lane(
                                    &l.name,
                                    run_id,
                                    AgentMessage::Tool {
                                        tool_call_id: tool.tool_call_id.clone(),
                                        name: tool.tool_name.clone(),
                                        content: error
                                            .clone()
                                            .unwrap_or_else(|| "Tool execution cancelled.".into()),
                                        is_error: true,
                                        terminate: false,
                                    },
                                )?;
                                any_provisioned = true;
                            }
                        }
                    }
                }
            }
            // The executor writes effects to disk immediately without updating the in-memory
            // AgentHarness store. Refresh so subsequent procedures see the current file state.
            if any_provisioned {
                let _ = self.refresh();
            }
            let _ = self.store.request_abort(run_id);
            let _ = self.store.drive_to_completion();
            // Refresh again so reconcile_abort_run sees the AbortRequested record we just wrote.
            let _ = self.refresh();
            if self.store.reconcile_abort_run(run_id).is_ok() {
                let _ = self.store.drive_to_completion();
                return Ok(());
            }
        }

        let outcome = match outcome {
            OpOutcome::Completed => OperationOutcome::Completed,
            OpOutcome::Aborted => OperationOutcome::Aborted,
            OpOutcome::Failed => OperationOutcome::Failed,
            OpOutcome::Declined => OperationOutcome::Declined,
        };
        self.store
            .finish_operation(run_id, outcome, error)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn checkpoint(
        &mut self,
        lane: &str,
        run_id: &str,
        messages: &[AgentMessage],
    ) -> Result<(), String> {
        if messages.is_empty() {
            return Ok(());
        }
        self.refresh()?;
        for message in messages {
            self.append_message_to_lane(lane, run_id, message.clone())?;
        }
        Ok(())
    }

    fn append_message_to_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        message: AgentMessage,
    ) -> Result<String, String> {
        self.refresh()?;
        let prefix = format!("subagent-entry-{run_id}-");
        if matches!(
            message,
            AgentMessage::User { .. } | AgentMessage::Assistant { .. }
        ) {
            if let Some(entry) = self.store.entries().iter().rev().find(|entry| {
                entry.lane == lane && entry.id.starts_with(&prefix) && entry.message == message
            }) {
                return Ok(entry.id.clone());
            }
        }
        let ordinal = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.id.starts_with(&prefix))
            .count();
        let id = match &message {
            AgentMessage::Tool { tool_call_id, .. } => {
                format!("subagent-result-{run_id}-{tool_call_id}")
            }
            AgentMessage::Assistant { .. } => self
                .store
                .records()
                .iter()
                .filter_map(|record| match record {
                    HarnessRecord::StepAttempt {
                        run_id: record_run,
                        result_entry_id,
                        ..
                    } if record_run == run_id => Some(result_entry_id.clone()),
                    _ => None,
                })
                .next()
                .unwrap_or_else(|| format!("{prefix}{ordinal}")),
            _ => format!("{prefix}{ordinal}"),
        };
        if let Some(entry) = self
            .store
            .entries()
            .iter()
            .find(|entry| entry.lane == lane && entry.id == id)
        {
            return Ok(entry.id.clone());
        }
        let parent_id = match &message {
            AgentMessage::Tool { tool_call_id, .. } => self
                .store
                .records()
                .iter()
                .rev()
                .find_map(|record| match record {
                    HarnessRecord::ToolStarted {
                        tool_call_id: id,
                        assistant_entry_id,
                        ..
                    } if id == tool_call_id => Some(assistant_entry_id.clone()),
                    _ => None,
                })
                .or_else(|| {
                    Reducer::reduce(self.store.store())
                        .ok()
                        .and_then(|state| state.lane(lane).and_then(|l| l.leaf_id.clone()))
                }),
            _ => Reducer::reduce(self.store.store())
                .ok()
                .and_then(|state| state.lane(lane).and_then(|l| l.leaf_id.clone()))
                .or_else(|| {
                    self.store
                        .entries()
                        .iter()
                        .rev()
                        .find(|e| e.lane == lane)
                        .map(|e| e.id.clone())
                }),
        };
        let seq = harness_next_seq(self.store.store());
        let terminate = matches!(
            &message,
            AgentMessage::Tool {
                terminate: true,
                ..
            }
        );
        let entry = threadlane_agent::harness::Entry {
            id: id.clone(),
            seq,
            lane: lane.into(),
            parent_id,
            timestamp: timestamp(),
            message,
            terminate,
        };
        self.store
            .append_entry_gated(entry)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn tool_started_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.refresh()?;
        let result_entry_id = format!("subagent-result-{run_id}-{tool_call_id}");
        let assistant_entry_id = match self
            .store
            .entries()
            .iter()
            .rev()
            .find(|entry| {
                entry.lane == lane && matches!(entry.message, AgentMessage::Assistant { .. })
            })
            .map(|entry| entry.id.clone())
        {
            Some(id) => id,
            None => {
                let assistant_msg = AgentMessage::Assistant {
                    content: None,
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                };
                self.append_message_to_lane(lane, run_id, assistant_msg)?
            }
        };
        let tool_index = self
            .store
            .records()
            .iter()
            .filter(|record| match record {
                HarnessRecord::ToolStarted {
                    run_id: r_id,
                    lane: r_lane,
                    ..
                } => r_id == run_id && r_lane == lane,
                _ => false,
            })
            .count();
        let record = HarnessRecord::ToolStarted {
            id: format!("tool-started-{run_id}-{tool_call_id}"),
            seq: harness_next_seq(self.store.store()),
            lane: lane.into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            assistant_entry_id,
            tool_index,
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            effective_args,
            result_entry_id,
            replay: match threadlane_agent::classify_tool_replay_safety(tool_name) {
                threadlane_agent::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                threadlane_agent::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
            },
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn claim_safe_replays(&mut self, tools: &[OpRecord]) -> Result<Vec<OpRecord>, String> {
        let records = self.store.records().to_vec();
        let entries = self.store.entries().to_vec();
        let mut claimed = Vec::new();
        for tool in tools {
            let OpRecord::ToolStarted {
                lane,
                run_id,
                assistant_entry_id,
                tool_index,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                replay: threadlane_agent::ToolReplaySafety::Safe,
                ..
            } = tool
            else {
                continue;
            };
            let already_completed =
                records.iter().any(|record| {
                    matches!(
                        record,
                        HarnessRecord::ToolFinished {
                            tool_call_id: finished_call,
                            ..
                        } if finished_call == tool_call_id
                    )
                }) || entries.iter().any(|entry| entry.id.contains(tool_call_id));
            if already_completed {
                continue;
            }
            let seq = self.next_seq();
            self.store
                .append_record_gated(HarnessRecord::ToolStarted {
                    id: format!("replay-claim-{run_id}-{tool_call_id}-{seq}"),
                    seq,
                    lane: lane.clone(),
                    timestamp: timestamp(),
                    run_id: run_id.clone(),
                    assistant_entry_id: assistant_entry_id.clone(),
                    tool_index: *tool_index,
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    effective_args: effective_args.clone(),
                    result_entry_id: result_entry_id.clone(),
                    replay: HarnessToolReplaySafety::Never,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            claimed.push(tool.clone());
        }
        Ok(claimed)
    }
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

const SUBAGENT_TOOL_NAME: &str = "subagent";

#[derive(Clone)]
pub struct SubagentToolExecutor {
    runner: AgentRunner,
}

impl SubagentToolExecutor {
    fn new(runner: AgentRunner) -> Self {
        Self { runner }
    }
}

fn subagent_tool_definition() -> AgentToolDefinition {
    AgentToolDefinition {
        name: SUBAGENT_TOOL_NAME.to_string(),
        description: Some(
            "Delegate one or more tasks to subagents in parallel or sequentially to complete work faster. Model can specify agent role, task prompt, custom instructions/system prompt, tool whitelist, and model override.".to_string(),
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "Ordered subagent tasks to run.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": {
                                "type": "string",
                                "description": "Subagent role or preset name (e.g. scout, worker, reviewer, planner, code_editor)."
                            },
                            "task": {
                                "type": "string",
                                "description": "Task description / prompt. In sequential mode, {previous} is replaced with the prior result."
                            },
                            "instructions": {
                                "type": "string",
                                "description": "Optional dynamic system instructions/prompt generated by the model for this subagent."
                            },
                            "tools": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional whitelist of tool names exposed to this subagent (e.g. ['read_file', 'edit_file_hashline'])."
                            },
                            "model": {
                                "type": "string",
                                "description": "Optional model override. Omit this field to inherit the parent session's active model (strongly recommended)."
                            }
                        },
                        "required": ["agent", "task"]
                    }
                },
                "parallel": {
                    "type": "boolean",
                    "description": "Set to true to run tasks concurrently in parallel, false for a sequential chain."
                }
            },
            "required": ["tasks"]
        }),
        strict: None,
    }
}

impl SubagentToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.subagent"
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        vec![subagent_tool_definition()]
    }

    async fn execute_tool_impl(
        &self,
        name: &str,
        args: &str,
        tool_call_id: Option<String>,
    ) -> Option<Result<String, String>> {
        if name != SUBAGENT_TOOL_NAME {
            return None;
        }

        let parsed: Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(err) => return Some(Err(format!("Invalid subagent tool arguments: {err}"))),
        };

        let tasks_val = match parsed.get("tasks").and_then(Value::as_array) {
            Some(arr) => arr,
            None => return Some(Err("Missing required argument `tasks`".into())),
        };
        if tasks_val.is_empty() {
            return Some(Err("`subagent` requires at least one task".into()));
        }
        if tasks_val.len() > MAX_SUBAGENT_TASKS {
            return Some(Err(format!(
                "`subagent` accepts at most {MAX_SUBAGENT_TASKS} tasks"
            )));
        }

        let mut tasks = Vec::new();
        for val in tasks_val {
            let agent = match val
                .get("agent")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(a) => a,
                None => return Some(Err("Each subagent task requires a non-empty `agent`".into())),
            };
            let task = match val
                .get("task")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(t) => t,
                None => return Some(Err("Each subagent task requires a non-empty `task`".into())),
            };
            let instructions = val
                .get("instructions")
                .or_else(|| val.get("system_prompt"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            let tools = val.get("tools").and_then(Value::as_array).map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            });
            let model = val
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);

            tasks.push(AgentRunTask {
                agent: agent.to_string(),
                task: task.to_string(),
                instructions,
                tools,
                model,
            });
        }

        let parallel = parsed
            .get("parallel")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        match (self.runner)(tasks, parallel, tool_call_id).await {
            Ok(val) => {
                let msg = val
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Subagents completed successfully.");
                Some(Ok(msg.to_string()))
            }
            Err(err) => Some(Err(err)),
        }
    }
}

#[async_trait]
impl ToolExecutor for SubagentToolExecutor {
    fn executor_id(&self) -> &str {
        SubagentToolExecutor::executor_id(self)
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        SubagentToolExecutor::tool_definitions(self)
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        self.execute_tool_impl(name, args, None).await
    }

    async fn execute_tool_with_call(
        &self,
        call: &AgentToolCall,
        args: &str,
    ) -> Option<Result<String, String>> {
        self.execute_tool_impl(&call.name, args, Some(call.id.clone()))
            .await
    }
}

fn render_agent_catalog(work_dir: &Path) -> String {
    let mut agents = discover_agents(work_dir, AgentScope::Both).agents;
    agents.sort_by(|left, right| left.name.cmp(&right.name));
    agents.truncate(32);

    let mut catalog = String::from(
        "=== Subagent Task Execution ===\nYou have access to the native `subagent` tool to spin off subagents in parallel or sequentially. You can use preset agent roles or spin dynamic subagents on the fly by providing custom `instructions` (system prompt), a whitelist of `tools`, and an optional `model` override for each task.\n\nPrefer delegating subtasks (research, searching, edits, tests, code reviews) to subagents so work finishes faster in parallel.\n",
    );
    if !agents.is_empty() {
        catalog.push_str("\nAvailable Preset Agent Roles:\n");
        for agent in agents {
            let description = agent
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let description: String = description.chars().take(240).collect();
            let name: String = agent.name.chars().take(128).collect();
            catalog.push_str(&format!("\n- `{}`: {}", name, description));
        }
    }
    catalog
}

fn restored_tool_policy(extensions: &WasiExtensionManager) -> ToolPolicy {
    match extensions
        .host_state("tools.policy")
        .and_then(|value| value.as_str().map(str::to_owned))
        .as_deref()
    {
        Some("read_only") => ToolPolicy::ReadOnly,
        _ => ToolPolicy::FullAccess,
    }
}

fn build_broker_dispatcher(
    tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    extensions: Arc<WasiExtensionManager>,
    persist_tool_policy: bool,
    work_dir: PathBuf,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    agent_work: AgentWorkScheduler,
    agent_runner: Option<AgentRunner>,
) -> (Arc<CapabilityDispatcher>, ManagedProcessRegistry) {
    let allowed_hosts: Arc<HashSet<String>> = Arc::new(
        std::env::var("THREADLANE_NETWORK_ALLOW_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_ascii_lowercase)
            .collect(),
    );
    let mut dispatcher = CapabilityDispatcher::new();
    let managed_processes = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    for capability in [
        "tools", "agent", "session", "fs", "process", "network", "ui", "events",
    ] {
        dispatcher.register(
            capability,
            Arc::new(HostCapabilityHandler {
                capability,
                tool_policy: Some(tool_policy.clone()),
                extensions: extensions.clone(),
                work_dir: work_dir.clone(),
                event_tx: event_tx.clone(),
                allowed_hosts: allowed_hosts.clone(),
                agent_work: agent_work.clone(),
                agent_runner: agent_runner.clone(),
                persist_tool_policy,
                managed_processes: managed_processes.clone(),
            }),
        );
    }
    (Arc::new(dispatcher), managed_processes)
}

async fn dispatch_hook_requests(
    dispatcher: &Arc<CapabilityDispatcher>,
    extensions: &WasiExtensionManager,
    requests: Vec<crate::extension_broker::HostBrokerRequest>,
) -> Result<(), BrokerError> {
    for request in requests {
        let dispatch = dispatcher.dispatch_envelopes(vec![request]).await?;
        extensions.enqueue_broker_results(dispatch.operation_results);
    }
    Ok(())
}

async fn dispatch_hook_requests_isolated(
    dispatcher: &Arc<CapabilityDispatcher>,
    extensions: &WasiExtensionManager,
    requests: Vec<crate::extension_broker::HostBrokerRequest>,
    label: &str,
) {
    for request in requests {
        if let Err(error) = dispatch_hook_requests(dispatcher, extensions, vec![request]).await {
            eprintln!("{label}: {}", error.message);
        }
    }
}

impl CodingAgent {
    pub(crate) fn set_tool_intent_recorder(
        &mut self,
        recorder: Option<threadlane_agent::ToolIntentRecorder>,
    ) {
        self.agent.loop_engine.tool_intent_recorder = recorder;
    }

    pub(crate) fn set_tool_completion_recorder(
        &mut self,
        recorder: Option<threadlane_agent::ToolCompletionRecorder>,
    ) {
        self.agent.loop_engine.tool_completion_recorder = recorder;
    }

    fn begin_harness_run(&mut self, prompt: AgentMessage) -> Result<Option<String>, String> {
        if let Some(run_id) = self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())?
            .clone()
        {
            return Ok(Some(run_id));
        }
        let Some(journal) = self.harness_journal.as_mut() else {
            return Ok(None);
        };
        let run_id = journal.unique_run_id("foreground")?;
        journal.start_with_prompt(&run_id, prompt)?;
        let context = HookContext {
            session_id: journal.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.clone()),
            resume_data: None,
        };
        for failure in journal.store.hooks().run(HookKind::BeforeRun, &context) {
            eprintln!("before-run hook {} failed: {}", failure.id, failure.message);
        }
        *self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())? = Some(run_id.clone());
        Ok(Some(run_id))
    }

    pub(crate) fn adopt_harness_run(&mut self, run_id: &str) -> Result<(), String> {
        let Some(journal) = self.harness_journal.as_mut() else {
            return Ok(());
        };
        journal.refresh()?;
        let state = Reducer::reduce(&journal.store).map_err(|error| error.to_string())?;
        let Some(open_run) = state
            .lane("main")
            .and_then(|lane| lane.open_operation.as_deref())
        else {
            return Err(format!("harness operation {run_id} is not open on main"));
        };
        if open_run != run_id {
            return Err(format!("harness operation {run_id} is not open on main"));
        }
        if let Some(path) = self.session_tree.file_path.clone() {
            self.session_tree = SessionTree::load_from_file(&path)
                .map_err(|error| format!("failed to refresh adopted session: {error}"))?;
            let prompt_entry_id = format!("entry-{run_id}-user");
            if self.session_tree.nodes.contains_key(&prompt_entry_id) {
                self.session_tree.switch_active_node(&prompt_entry_id);
            }
        }
        *self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())? = Some(run_id.into());
        Ok(())
    }

    fn finish_harness_run(
        &mut self,
        run_id: Option<&str>,
        outcome: OpOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        let (Some(journal), Some(run_id)) = (self.harness_journal.as_mut(), run_id) else {
            return Ok(());
        };
        let result = journal.finish(run_id, outcome, error);
        if result.is_ok() {
            let context = HookContext {
                session_id: journal.store.session_id().to_owned(),
                lane: "main".into(),
                run_id: Some(run_id.into()),
                resume_data: None,
            };
            for failure in journal.store.hooks().run(HookKind::AfterRun, &context) {
                eprintln!("after-run hook {} failed: {}", failure.id, failure.message);
            }
        }
        if let Ok(mut active) = self.harness_run_id.lock() {
            if active.as_deref() == Some(run_id) {
                *active = None;
            }
        }
        result
    }

    fn append_command_message(&mut self, message: AgentMessage) -> Result<(), String> {
        if let Some(journal) = self.harness_journal.as_mut() {
            journal.append_message(message.clone())?;
            self.session_tree.add_message_in_memory(message);
        } else {
            self.session_tree.add_message(message);
        }
        Ok(())
    }

    async fn compact_history_with_harness(&mut self) -> Result<bool, String> {
        if !self.agent.compact_history(None).await {
            return Ok(false);
        }
        let state = self.agent.get_state().await;
        let summary = state
            .messages
            .iter()
            .rev()
            .find_map(threadlane_agent::compaction_summary_text)
            .ok_or_else(|| "compaction produced no durable summary".to_string())?;
        let retained_tail = compaction_retained_tail(&state.messages);
        self.persist_harness_compaction(summary, &retained_tail)?;
        if self.harness_journal.is_some() {
            let path = self
                .session_tree
                .file_path
                .clone()
                .ok_or_else(|| "harness compaction has no session path".to_string())?;
            self.session_tree = SessionTree::load_from_file(&path)
                .map_err(|error| format!("failed to reload compacted session: {error}"))?;
        } else {
            self.session_tree.replace_active_branch(state.messages);
        }
        Ok(true)
    }

    fn persist_harness_compaction(
        &mut self,
        summary: &str,
        retained_tail: &[AgentMessage],
    ) -> Result<(), String> {
        if let Some(journal) = self.harness_journal.as_mut() {
            journal.refresh()?;
            let run_id = journal.unique_run_id("foreground-compaction")?;
            journal
                .store
                .accept_compaction(&run_id, summary)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            for message in retained_tail {
                journal.append_message(message.clone())?;
            }
        }
        Ok(())
    }

    async fn navigate_tree_branch(&mut self, node_id: &str) -> Result<String, String> {
        if !self.session_tree.nodes.contains_key(node_id) {
            return Err(format!("Node ID not found in session tree: {node_id}"));
        }
        let mut branch_ids = Vec::new();
        let mut current = Some(node_id.to_owned());
        while let Some(id) = current {
            let node = self
                .session_tree
                .nodes
                .get(&id)
                .ok_or_else(|| format!("Node ID not found in session tree: {id}"))?;
            branch_ids.push(id);
            current = node.parent_id.clone();
        }
        branch_ids.reverse();
        let mut harness_target_id = None;
        if let Some(journal) = self.harness_journal.as_mut() {
            journal.refresh()?;
            let mut parent_id = None;
            for legacy_id in branch_ids {
                let node = self
                    .session_tree
                    .nodes
                    .get(&legacy_id)
                    .ok_or_else(|| format!("Node ID not found in session tree: {legacy_id}"))?;
                if matches!(node.message, AgentMessage::System { .. }) {
                    continue;
                }
                let entry_id = if journal
                    .store
                    .entries()
                    .iter()
                    .any(|entry| entry.id == legacy_id)
                {
                    legacy_id.clone()
                } else {
                    format!("v2-navigation-{legacy_id}")
                };
                if !journal
                    .store
                    .entries()
                    .iter()
                    .any(|entry| entry.id == entry_id)
                {
                    journal
                        .store
                        .append_entry_gated(HarnessEntry {
                            id: entry_id.clone(),
                            parent_id: parent_id.clone(),
                            lane: "main".into(),
                            seq: harness_next_seq(journal.store.store()),
                            timestamp: now_millis(),
                            message: node.message.clone(),
                            terminate: matches!(
                                node.message,
                                AgentMessage::Tool {
                                    terminate: true,
                                    ..
                                }
                            ),
                        })
                        .map_err(|error| error.to_string())?;
                    journal
                        .store
                        .drive_to_completion()
                        .map_err(|error| error.to_string())?;
                }
                parent_id = Some(entry_id.clone());
                if legacy_id == node_id {
                    harness_target_id = Some(entry_id);
                }
            }
            let target_entry_id = harness_target_id.ok_or_else(|| {
                "navigation target was not materialized in the harness".to_string()
            })?;
            let run_id = journal.unique_run_id("foreground-navigation")?;
            journal
                .store
                .accept_navigation(&run_id, &target_entry_id, None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            if let Some(path) = self.session_tree.file_path.clone() {
                self.session_tree = SessionTree::load_from_file(&path)
                    .map_err(|error| format!("failed to reload navigated session: {error}"))?;
                self.session_tree.switch_active_node(&target_entry_id);
                let branch_msgs = self.session_tree.get_active_branch_messages();
                let mut agent_state = self.agent.loop_engine.state.lock().await;
                agent_state.messages = branch_msgs;
                return Ok(format!("Switched session tree to node: {node_id}"));
            }
        }
        if self.session_tree.switch_active_node(node_id) {
            let branch_msgs = self.session_tree.get_active_branch_messages();
            let mut agent_state = self.agent.loop_engine.state.lock().await;
            agent_state.messages = branch_msgs;
            Ok(format!("Switched session tree to node: {node_id}"))
        } else {
            Err(format!("Node ID not found in session tree: {node_id}"))
        }
    }

    pub fn set_credentials(&mut self, api_key: String, account_id: Option<String>) {
        self.agent.loop_engine.set_credentials(api_key, account_id);
    }

    pub async fn replay_safe_tools(
        &self,
        records: &[threadlane_agent::OpRecord],
    ) -> Vec<AgentToolResult> {
        let calls = records
            .iter()
            .filter_map(|record| match record {
                threadlane_agent::OpRecord::ToolStarted {
                    tool_call_id,
                    tool_name,
                    effective_args,
                    replay: threadlane_agent::ToolReplaySafety::Safe,
                    ..
                } => Some(threadlane_provider::openai::ToolCall {
                    id: tool_call_id.clone(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: tool_name.clone(),
                        arguments: effective_args.to_string(),
                    },
                    thought_signature: None,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        if calls.is_empty() {
            return Vec::new();
        }
        self.agent
            .loop_engine
            .execute_tools_for_replay(&calls)
            .await
    }

    pub async fn sync_session_history(&mut self) {
        let branch = self.session_tree.get_active_branch_messages();
        let mut state = self.agent.loop_engine.state.lock().await;
        let system_prompt = state.system_prompt.clone();
        state.messages = std::iter::once(AgentMessage::System {
            content: system_prompt,
        })
        .chain(
            branch
                .into_iter()
                .filter(|message| !matches!(message, AgentMessage::System { .. })),
        )
        .collect();
    }

    pub async fn reload_extensions(&mut self) -> Result<usize, String> {
        let global_threadlane_dir = default_global_threadlane_dir();
        let loaded = self
            .wasi_extensions
            .reload_from_roots(global_threadlane_dir.as_deref(), Some(&self.work_dir))?;
        self.managed_processes.lock().await.clear();
        Ok(loaded)
    }

    /// Rediscover skills for this project, applying any persisted enable/disable
    /// overrides, and refresh the shared registry and the model-facing system prompt.
    pub fn refresh_skills(&mut self) {
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&self.work_dir));
        let skills = skill_manager.snapshot();
        self.skills = skills;
    }

    pub async fn refresh_mcp(&self) {
        self.mcp_manager.discover_and_connect().await;
    }

    pub fn new(options: CodingAgentOptions) -> Self {
        let project_context = ProjectContext::discover(&options.work_dir);
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&options.work_dir));
        let skills = skill_manager.snapshot();
        let skill_catalog = skills.render_model_catalog();

        // A missing session file represents an unsaved draft. GUI startup uses
        // this mode so merely opening the app neither creates nor selects a
        // conversation; the first send binds the draft to a new session.
        let mut session_tree = if let Some(session_path) = options.session_file.clone() {
            let session_id = || {
                session_path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "session".into())
            };
            if session_path.exists() {
                SessionTree::load_from_file(&session_path).unwrap_or_else(|_| {
                    let mut session = SessionTree::new(session_id());
                    session.file_path = Some(session_path.clone());
                    session
                })
            } else {
                if let Some(parent) = session_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let mut session = SessionTree::new(session_id());
                session.file_path = Some(session_path);
                session
            }
        } else {
            SessionTree::new("draft")
        };
        let mut effective_model = session_tree
            .model
            .clone()
            .unwrap_or_else(|| options.model.clone());
        let (harness_journal, harness_journal_error) = match session_tree.file_path.as_deref() {
            Some(path) => match HarnessJournal::open(path) {
                Ok(journal) => (Some(journal), None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        if let Some(journal) = harness_journal.as_ref() {
            if let Some(model) = journal.store.facts().get("model") {
                effective_model = model.clone();
                session_tree.model = Some(model.clone());
            }
            if let Some(name) = journal.store.facts().get("name") {
                session_tree.name = Some(name.clone());
            }
            // V2 owns the durable active leaf. Legacy metadata may still
            // point at the previous turn, which makes a reopened session hide
            // prompts that were already committed to the harness journal.
            let has_v2_main_records = journal
                .store
                .records()
                .iter()
                .any(|record| record.lane() == "main");
            if has_v2_main_records {
                if let Ok(state) = Reducer::reduce(&journal.store) {
                    if let Some(leaf_id) =
                        state.lane("main").and_then(|lane| lane.leaf_id.as_deref())
                    {
                        if session_tree.nodes.contains_key(leaf_id) {
                            session_tree.switch_active_node(leaf_id);
                        }
                    }
                }
            }
        }
        session_tree
            .model
            .get_or_insert_with(|| effective_model.clone());
        let has_interrupted_subagents = match harness_journal.as_ref() {
            Some(_journal) => {
                let records = session_tree
                    .file_path
                    .as_deref()
                    .map(|path| recover_v2_subagent_records(path).unwrap_or_default())
                    .unwrap_or_default();
                !threadlane_agent::interrupted_subagent_lanes(&records).is_empty()
            }
            None => session_tree.file_path.is_some(),
        };
        let interrupted_subagent_recovery = if has_interrupted_subagents {
            InterruptedSubagentRecoveryState::Pending
        } else {
            InterruptedSubagentRecoveryState::Complete
        };
        let plan_store =
            SessionPlanStore::new(session_tree.plan().clone(), session_tree.file_path.clone());
        let mut agent = Agent::new(&options.api_key, options.account_id, &effective_model);
        let harness_run_id: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        if let Some(path) = session_tree.file_path.clone() {
            let message_path = path.clone();
            let active_run = harness_run_id.clone();
            agent.loop_engine.assistant_message_recorder = Some(Arc::new(move |message| {
                let path = message_path.clone();
                let active_run = active_run.clone();
                let active = active_run
                    .lock()
                    .ok()
                    .is_some_and(|run_id| run_id.is_some());
                Box::pin(async move {
                    if active {
                        HarnessJournal::append_message_to_path(&path, message)
                    } else {
                        Ok(())
                    }
                })
            }));

            let tool_message_path = path.clone();
            let active_run = harness_run_id.clone();
            agent.loop_engine.tool_message_recorder = Some(Arc::new(move |message| {
                let path = tool_message_path.clone();
                let run_id = active_run.lock().ok().and_then(|run_id| run_id.clone());
                Box::pin(async move {
                    if let Some(run_id) = run_id {
                        let mut journal = HarnessJournal::open(&path)?;
                        journal.append_message(message.clone())?;
                        journal.finish_tool_message(&run_id, &message)?;
                        journal.run_after_tool_hook(&run_id);
                        Ok(())
                    } else {
                        Ok(())
                    }
                })
            }));

            let usage_path = path.clone();
            let active_run = harness_run_id.clone();
            agent.loop_engine.provider_usage_recorder = Some(Arc::new(move |usage| {
                let path = usage_path.clone();
                let run_id = active_run.lock().ok().and_then(|run_id| run_id.clone());
                Box::pin(async move {
                    let Some(run_id) = run_id else {
                        return Ok(());
                    };
                    let mut journal = HarnessJournal::open(&path)?;
                    journal.record_provider_usage(&run_id, usage)
                })
            }));

            let discarded_usage_path = path.clone();
            let active_run = harness_run_id.clone();
            agent.loop_engine.provider_discarded_usage_recorder = Some(Arc::new(move |usage| {
                let path = discarded_usage_path.clone();
                let run_id = active_run.lock().ok().and_then(|run_id| run_id.clone());
                Box::pin(async move {
                    let Some(run_id) = run_id else {
                        return Ok(());
                    };
                    let mut journal = HarnessJournal::open(&path)?;
                    journal.record_discarded_usage(&run_id, usage)
                })
            }));

            let streaming_path = path.clone();
            let active_run = harness_run_id.clone();
            agent.loop_engine.streaming_state_recorder = Some(Arc::new(move |mut state| {
                let path = streaming_path.clone();
                let run_id = active_run.lock().ok().and_then(|run_id| run_id.clone());
                Box::pin(async move {
                    let empty = state.assistant_text.is_empty()
                        && state.reasoning.is_empty()
                        && state.tool_call_ids.is_empty();
                    if empty {
                        harness_event_hub(&path).publish_streaming(None);
                    } else {
                        state.lane = "main".into();
                        state.run_id = run_id;
                        harness_event_hub(&path).publish_streaming(Some(state));
                    }
                    Ok(())
                })
            }));

            let hook_path = path.clone();
            let active_run = harness_run_id.clone();
            agent.loop_engine.provider_hook_recorder = Some(Arc::new(move |kind| {
                let path = hook_path.clone();
                let run_id = active_run.lock().ok().and_then(|run_id| run_id.clone());
                Box::pin(async move {
                    let Some(run_id) = run_id else {
                        return Ok(Vec::new());
                    };
                    let mut journal = HarnessJournal::open(&path)?;
                    if kind == HookKind::BeforeRequest {
                        journal.prepare_assistant_attempt(&run_id)?;
                    }
                    let context = HookContext {
                        session_id: journal.store.session_id().to_owned(),
                        lane: "main".into(),
                        run_id: Some(run_id),
                        resume_data: None,
                    };
                    Ok(journal
                        .store
                        .hooks()
                        .run(kind, &context)
                        .into_iter()
                        .map(|failure| format!("{}: {}", failure.id, failure.message))
                        .collect())
                })
            }));

            let intent_path = path;
            let active_run = harness_run_id.clone();
            agent.loop_engine.tool_intent_recorder =
                Some(Arc::new(move |tool_call_id, tool_name, arguments| {
                    let path = intent_path.clone();
                    let run_id = active_run.lock().ok().and_then(|run_id| run_id.clone());
                    let tool_call_id = tool_call_id.to_owned();
                    let tool_name = tool_name.to_owned();
                    let arguments = arguments.to_owned();
                    Box::pin(async move {
                        let Some(run_id) = run_id else {
                            return Ok(());
                        };
                        let effective_args = serde_json::from_str(&arguments).map_err(|error| {
                            format!("invalid normalized tool arguments: {error}")
                        })?;
                        HarnessJournal::append_tool_intent_to_path(
                            &path,
                            &run_id,
                            &tool_call_id,
                            &tool_name,
                            effective_args,
                        )
                    })
                }));
        }
        let cancellation = CodingAgentCancellation {
            state: Arc::default(),
            harness_session_file: session_tree.file_path.clone(),
            event_tx: agent.loop_engine.event_tx.clone(),
        };

        agent.set_prompt_cache_key(Some(session_tree.session_id.clone()));

        let wasi_extensions = WasiExtensionManager::for_project_session(
            &options.work_dir,
            session_tree.session_id.clone(),
        );
        let global_threadlane_dir = default_global_threadlane_dir();
        let loaded_ext_count = wasi_extensions
            .reload_from_roots(global_threadlane_dir.as_deref(), Some(&options.work_dir))
            .unwrap_or_default();
        let agent_catalog = render_agent_catalog(&options.work_dir);
        let initial_tool_policy = restored_tool_policy(&wasi_extensions);
        let tool_policy = Arc::new(tokio::sync::Mutex::new(initial_tool_policy));
        let wasi_extensions = Arc::new(wasi_extensions);
        let agent_work = AgentWorkScheduler::default();
        #[cfg(test)]
        let subagent_work_observer = Arc::new(std::sync::Mutex::new(None));
        #[cfg(test)]
        let runner_observer: Option<SubagentObserverState> = Some(subagent_work_observer.clone());
        let runner_api_key = agent.loop_engine.api_key.clone();
        let runner_account_id = agent.loop_engine.account_id.clone();
        let runner_state = agent.loop_engine.state.clone();
        let runner_work_dir = options.work_dir.clone();
        let runner_extensions = wasi_extensions.clone();
        let runner_event_tx = agent.loop_engine.event_tx.clone();
        let runner_session_file = session_tree.file_path.clone();
        let runner_semaphore = Arc::new(tokio::sync::Semaphore::new(SUBAGENT_CONCURRENCY_LIMIT));
        let dispatch_parent_leaf = Arc::new(std::sync::Mutex::new(None));
        let completed_subagent_lanes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner_parent_leaf = dispatch_parent_leaf.clone();
        let runner_completed_lanes = completed_subagent_lanes.clone();
        let parent_session_id = session_tree.session_id.clone();
        let agent_runner: AgentRunner = Arc::new(move |tasks, parallel, tool_call_id| {
            #[cfg(test)]
            let observer = runner_observer.clone();
            let api_key = runner_api_key.clone();
            let account_id = runner_account_id.clone();
            let state = runner_state.clone();
            let work_dir = runner_work_dir.clone();
            let extensions = runner_extensions.clone();
            let event_tx = runner_event_tx.clone();
            let session_file = runner_session_file.clone();
            let semaphore = runner_semaphore.clone();
            let parent_leaf_id = runner_parent_leaf.lock().ok().and_then(|leaf| leaf.clone());
            let completed_lanes = runner_completed_lanes.clone();
            let parent_session_id = parent_session_id.clone();
            Box::pin(async move {
                let model = state.lock().await.model.clone();
                #[cfg(test)]
                let observer = observer
                    .and_then(|observer| observer.lock().ok().and_then(|value| value.clone()));
                let (output, thinking, lanes) = run_subagents_with_context(
                    tasks,
                    parallel,
                    tool_call_id,
                    SubagentRunContext {
                        api_key,
                        account_id,
                        parent_model: model,
                        parent_session_id: parent_session_id.clone(),
                        work_dir,
                        extensions,
                        parent_event_tx: event_tx,
                        parent_leaf_id,
                        session_file,
                        #[cfg(test)]
                        scheduler_observer: observer,
                        #[cfg(test)]
                        child_work_observer: None,
                        #[cfg(test)]
                        child_tool_observer: None,
                        semaphore,
                    },
                )
                .await?;
                accept_completed_subagent_lanes(&completed_lanes, lanes)?;
                Ok(serde_json::json!({
                    "message": output,
                    "output": output,
                    "thinking": thinking
                }))
            })
        });
        let (broker_dispatcher, managed_processes) = build_broker_dispatcher(
            tool_policy.clone(),
            wasi_extensions.clone(),
            true,
            options.work_dir.clone(),
            agent.loop_engine.event_tx.clone(),
            agent_work.clone(),
            Some(agent_runner.clone()),
        );
        agent
            .loop_engine
            .register_tool_executor(Arc::new(LoadSkillToolExecutor::new(skills.clone())))
            .expect("reserved load_skill tool must register");
        agent
            .loop_engine
            .register_tool_executor(Arc::new(SubagentToolExecutor::new(agent_runner.clone())))
            .expect("reserved subagent tool must register");
        agent
            .loop_engine
            .register_tool_executor(Arc::new(UpdatePlanToolExecutor::new(
                plan_store.clone(),
                agent.loop_engine.event_tx.clone(),
            )))
            .expect("reserved update_plan tool must register");
        if let Err(error) =
            agent
                .loop_engine
                .register_tool_executor(Arc::new(BrokerAwareWasiToolExecutor {
                    extensions: wasi_extensions.clone(),
                    broker_dispatcher: broker_dispatcher.clone(),
                }))
        {
            eprintln!("WASI tool registration failed: {error}");
        }
        let mcp_manager = Arc::new(McpManager::new(
            default_global_threadlane_dir(),
            Some(options.work_dir.clone()),
        ));
        let manager_clone = mcp_manager.clone();
        threadlane_agent::get_runtime().spawn(async move {
            manager_clone.discover_and_connect().await;
        });
        if let Err(error) = agent
            .loop_engine
            .register_tool_executor(Arc::new(McpToolExecutor::new(mcp_manager.clone())))
        {
            eprintln!("MCP tool registration failed: {error}");
        }
        agent.loop_engine.work_dir = Some(options.work_dir.clone());

        let mut system_prompt_config = options.system_prompt.clone();
        if initial_tool_policy == ToolPolicy::ReadOnly {
            system_prompt_config.guidelines.push(
                "The current workspace tool policy is read-only; do not request file mutations or host commands."
                    .to_string(),
            );
        }
        let prompt_tools = agent.loop_engine.configured_tool_definitions();
        let base_system_prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &system_prompt_config,
            work_dir: &options.work_dir,
            tools: &prompt_tools,
            project_context: &project_context,
            skill_catalog: Some(&skill_catalog),
            agent_catalog: Some(&agent_catalog),
            loaded_extension_count: loaded_ext_count,
        });

        agent.loop_engine.before_tool_call_hook = Some(Arc::new(ExtensionBeforeToolHook {
            tool_policy: tool_policy.clone(),
            extensions: wasi_extensions.clone(),
            broker_dispatcher: broker_dispatcher.clone(),
        }));
        agent.loop_engine.after_tool_call_hook = Some(Arc::new(ExtensionAfterToolHook {
            extensions: wasi_extensions.clone(),
            broker_dispatcher: broker_dispatcher.clone(),
        }));

        {
            let mut state = agent
                .loop_engine
                .state
                .try_lock()
                .expect("Failed to lock initial state");
            state.system_prompt = base_system_prompt.clone();
            state.messages.push(AgentMessage::System {
                content: base_system_prompt.clone(),
            });
            state.messages.extend(
                session_tree
                    .get_active_branch_messages()
                    .into_iter()
                    .filter(|message| !matches!(message, AgentMessage::System { .. })),
            );
        }

        Self {
            agent,
            session_tree,
            wasi_extensions,
            tool_policy,
            work_dir: options.work_dir,
            skills,
            agent_runner,
            broker_dispatcher,
            managed_processes,
            agent_work,
            mcp_manager,
            plan_store,
            prompt_templates: None,
            dispatch_parent_leaf,
            completed_subagent_lanes,
            harness_journal,
            harness_journal_error,
            harness_run_id,
            cancellation,
            interrupted_subagent_recovery,
            #[cfg(test)]
            subagent_work_observer,
            #[cfg(test)]
            subagent_branch_observer: None,
        }
    }

    pub async fn run_scheduled_agent_work(&mut self) {
        while self.agent_work.run(&mut self.agent).await {
            self.sync_session_tree_and_dispatch_assistant_hooks().await;
            if let Some(path) = self.session_tree.file_path.as_deref() {
                if let Err(error) = consume_harness_follow_ups(path) {
                    eprintln!("Failed to consume queued follow-up: {error}");
                }
                if let Err(error) = consume_harness_queue(path, QueueKind::Steer) {
                    eprintln!("Failed to consume queued steer: {error}");
                }
                if let Err(error) = consume_harness_queue(path, QueueKind::NextRun) {
                    eprintln!("Failed to consume queued next-run input: {error}");
                }
            }
        }
    }

    pub fn work_handle(&self) -> CodingAgentWorkHandle {
        CodingAgentWorkHandle {
            scheduler: self.agent_work.clone(),
            session_file: self.session_tree.file_path.clone(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.agent.subscribe()
    }

    pub fn harness_snapshot(&mut self) -> Result<Option<Snapshot>, String> {
        let Some(journal) = self.harness_journal.as_mut() else {
            return Ok(None);
        };
        journal.refresh()?;
        journal
            .store
            .snapshot()
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub fn harness_error(&self) -> Option<&str> {
        self.harness_journal_error.as_deref()
    }

    pub fn watch_harness(&mut self) -> Result<Option<HarnessWatch>, String> {
        let Some(journal) = self.harness_journal.as_mut() else {
            return Ok(None);
        };
        journal.refresh()?;
        let subscription = journal
            .store
            .watch_session()
            .map_err(|error| error.to_string())?;
        Ok(Some(HarnessWatch {
            hub: journal.store.events().clone(),
            subscription,
        }))
    }

    pub fn cancellation_handle(&self) -> CodingAgentCancellation {
        self.cancellation.clone()
    }

    pub fn cancel(&self) -> Result<(), String> {
        self.cancellation.cancel()
    }

    pub fn current_plan(&self) -> threadlane_agent::SessionPlan {
        self.plan_store.current()
    }

    async fn dispatch_assistant_hook(&self, message: &AgentMessage) {
        let AgentMessage::Assistant {
            content,
            tool_calls,
            ..
        } = message
        else {
            return;
        };
        let arguments = serde_json::json!({
            "content": content,
            "tool_calls": tool_calls,
        });
        for response in self
            .wasi_extensions
            .execute_hook_with_effects("assistant_message", &arguments.to_string())
            .into_iter()
            .flatten()
        {
            let _ = dispatch_hook_requests(
                &self.broker_dispatcher,
                &self.wasi_extensions,
                response.host_broker_requests,
            )
            .await;
        }
        let _ = dispatch_hook_requests(
            &self.broker_dispatcher,
            &self.wasi_extensions,
            self.wasi_extensions.take_pending_broker_requests(),
        )
        .await;
    }

    async fn sync_session_tree_and_dispatch_assistant_hooks(&mut self) {
        let state = self.agent.get_state().await;
        let harness_persists_messages = self.harness_journal.is_some();

        // The loop engine keeps the complete provider conversation in memory,
        // including assistant tool-call messages and the tool results that
        // follow them. Persist the portion that is not in the session yet so
        // reloading a session produces the same provider history (and keeps
        // the tool-call/result ordering intact).
        let state_messages: Vec<AgentMessage> = state
            .messages
            .into_iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .collect();

        // V2 commits assistant and tool entries from the loop-engine
        // recorders. Re-running the legacy prefix diff here can dispatch hooks
        // twice and makes the UI tree a second persistence path.
        if harness_persists_messages {
            if let Some(path) = self.session_tree.file_path.clone() {
                match SessionTree::load_from_file(&path) {
                    Ok(tree) => self.session_tree = tree,
                    Err(error) => eprintln!("Failed to reload V2 session tree: {error}"),
                }
            }
            return;
        }

        let persisted_messages = self.session_tree.get_active_branch_messages();

        let common_prefix = state_messages
            .iter()
            .zip(persisted_messages.iter())
            .take_while(|(state_message, persisted_message)| {
                serde_json::to_value(state_message).ok()
                    == serde_json::to_value(persisted_message).ok()
            })
            .count();

        let start_index = if common_prefix == persisted_messages.len() {
            // Agent::prompt records the same user message that CodingAgent
            // already stored for normal prompts. Avoid storing that duplicate.
            if matches!(
                (state_messages.get(common_prefix), persisted_messages.last()),
                (Some(state_message), Some(persisted_message))
                    if state_message.same_user_message(persisted_message)
            ) {
                common_prefix + 1
            } else {
                common_prefix
            }
        } else if persisted_messages.len() == common_prefix + 1
            && state_messages
                .get(common_prefix)
                .is_some_and(AgentMessage::is_user)
        {
            // Skills and extensions store the visible command, then prompt
            // the model with a different, generated user message. Keep that
            // generated message so the restored provider history is exact.
            common_prefix
        } else if state_messages
            .iter()
            .any(|message| threadlane_agent::compaction_summary_text(message).is_some())
        {
            // Auto-compaction creates a new active root branch. Persist that
            // branch in-place instead of treating it as a new session.
            let current_turn_start = state_messages
                .iter()
                .rposition(AgentMessage::is_user)
                .unwrap_or(state_messages.len());
            for message in state_messages.iter().skip(current_turn_start + 1) {
                self.dispatch_assistant_hook(message).await;
            }
            if harness_persists_messages {
                let Some(path) = self.session_tree.file_path.clone() else {
                    eprintln!("Failed to reload compacted session: no session path");
                    return;
                };
                match SessionTree::load_from_file(&path) {
                    Ok(tree) => self.session_tree = tree,
                    Err(error) => eprintln!("Failed to reload compacted session: {error}"),
                }
            } else {
                self.session_tree.replace_active_branch(state_messages);
            }
            return;
        } else {
            // A non-prefix means the session was changed independently. Do
            // not append a second, potentially duplicated conversation.
            return;
        };

        for message in state_messages.into_iter().skip(start_index) {
            self.dispatch_assistant_hook(&message).await;
            if harness_persists_messages {
                self.session_tree.add_message_in_memory(message.clone());
            } else {
                self.session_tree.add_message(message.clone());
            }
        }
    }

    #[cfg(test)]
    fn set_subagent_work_observer(&self, observer: Arc<std::sync::Mutex<Vec<AgentWork>>>) {
        if let Ok(mut current) = self.subagent_work_observer.lock() {
            *current = Some(observer);
        }
    }

    #[cfg(test)]
    fn set_subagent_branch_observer(&mut self, observer: SubagentBoundaryObserver) {
        self.subagent_branch_observer = Some(observer);
    }

    fn commit_completed_subagent_lanes(&mut self) -> Result<(), String> {
        let lanes = {
            let mut completed = self
                .completed_subagent_lanes
                .lock()
                .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?;
            std::mem::take(&mut *completed)
        };
        for (index, lane) in lanes.iter().enumerate() {
            let status = match lane.status {
                SubagentLaneStatus::Completed => "completed",
                SubagentLaneStatus::Failed => "failed",
            };
            let mut messages = Vec::with_capacity(lane.messages.len() + 1);
            messages.push(AgentMessage::Custom {
                custom_type: "subagent_lane".into(),
                payload: serde_json::json!({
                    "lane": lane.lane_name,
                    "run_id": lane.run_id,
                    "agent": lane.agent,
                    "task": lane.task,
                    "status": status,
                    "error": lane.error,
                }),
            });
            messages.extend(lane.messages.clone());
            if let Err(error) = self
                .session_tree
                .append_passive_branch(lane.parent_leaf_id.as_deref(), messages)
            {
                self.completed_subagent_lanes
                    .lock()
                    .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?
                    .extend_from_slice(&lanes[index..]);
                return Err(error);
            }
            #[cfg(test)]
            if let Some(observer) = self.subagent_branch_observer.as_ref() {
                observer();
            }
        }
        for (index, lane) in lanes.iter().enumerate() {
            if let Some(path) = self.session_tree.file_path.as_deref() {
                let outcome = match lane.status {
                    SubagentLaneStatus::Completed => OpOutcome::Completed,
                    SubagentLaneStatus::Failed => OpOutcome::Failed,
                };
                let mut journal = HarnessJournal::open(path)?;
                if let Err(error) = journal.finish_subagent_lane(
                    &lane.lane_name,
                    &lane.run_id,
                    outcome,
                    lane.error.clone(),
                ) {
                    self.completed_subagent_lanes
                        .lock()
                        .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?
                        .extend_from_slice(&lanes[index..]);
                    self.interrupted_subagent_recovery = InterruptedSubagentRecoveryState::Pending;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    async fn recover_interrupted_subagent_lanes(&mut self) -> Result<usize, String> {
        match &self.interrupted_subagent_recovery {
            InterruptedSubagentRecoveryState::Complete => return Ok(0),
            InterruptedSubagentRecoveryState::Pending => {}
        }
        if let Some(error) = self.harness_journal_error.as_ref() {
            return Err(format!("Harness Error: {error}"));
        }

        let result: Result<usize, String> = async {
            let path = self
                .session_tree
                .file_path
                .clone()
                .ok_or_else(|| "Interrupted subagent journal is unavailable".to_string())?;
            let records = recover_v2_subagent_records(&path).unwrap_or_default();
            let markers = self
                .session_tree
                .nodes
                .values()
                .filter_map(|node| match &node.message {
                    AgentMessage::Custom {
                        custom_type,
                        payload,
                    } if custom_type == "subagent_lane" => payload
                        .get("run_id")
                        .and_then(Value::as_str)
                        .and_then(|run_id| {
                            payload.get("lane").and_then(Value::as_str).map(|lane| {
                                (
                                    (lane.to_owned(), run_id.to_owned()),
                                    (
                                        node.id.clone(),
                                        payload
                                            .get("status")
                                            .and_then(Value::as_str)
                                            .unwrap_or("completed")
                                            .to_owned(),
                                        payload
                                            .get("error")
                                            .and_then(Value::as_str)
                                            .map(str::to_owned),
                                    ),
                                )
                            })
                        }),
                    _ => None,
                })
                .collect::<HashMap<_, _>>();
            let mut recovered = 0;

            for lane in threadlane_agent::interrupted_subagent_lanes(&records) {
                let retrying = |error: String| {
                    let _ = self
                        .agent
                        .loop_engine
                        .event_tx
                        .send(AgentEvent::SubagentRecovery {
                            run_id: lane.run_id.clone(),
                            status: SubagentRecoveryStatus::Retrying,
                            detail: Some("Recovery needs retry".into()),
                        });
                    error
                };
                let mut journal = HarnessJournal::open(&path).map_err(&retrying)?;
                let _ = self
                    .agent
                    .loop_engine
                    .event_tx
                    .send(AgentEvent::SubagentRecovery {
                        run_id: lane.run_id.clone(),
                        status: SubagentRecoveryStatus::Started,
                        detail: Some("Recovering interrupted task".into()),
                    });
                if !lane.task_attempted {
                    let error = "Interrupted subagent had no persisted task attempt".to_string();
                    let messages = vec![AgentMessage::Custom {
                        custom_type: "subagent_lane".into(),
                        payload: serde_json::json!({
                            "lane": lane.lane,
                            "run_id": lane.run_id,
                            "agent": "recovered",
                            "task": lane.task,
                            "status": "aborted",
                            "error": error,
                        }),
                    }];
                    self.session_tree
                        .append_passive_branch_in_memory(lane.source_leaf_id.as_deref(), messages)
                        .map_err(&retrying)?;
                    journal
                        .finish_subagent_lane(
                            &lane.lane,
                            &lane.run_id,
                            OpOutcome::Aborted,
                            Some(error),
                        )
                        .map_err(&retrying)?;
                    let _ = self
                        .agent
                        .loop_engine
                        .event_tx
                        .send(AgentEvent::SubagentRecovery {
                            run_id: lane.run_id.clone(),
                            status: SubagentRecoveryStatus::Aborted,
                            detail: Some("Interrupted task was not replayable".into()),
                        });
                    recovered += 1;
                    continue;
                }
                if let Some((marker_id, status, error)) =
                    markers.get(&(lane.lane.clone(), lane.run_id.clone()))
                {
                    let recorded = records
                        .iter()
                        .filter_map(|record| match record {
                            OpRecord::WriteDeferred {
                                lane: recorded_lane,
                                run_id,
                                target,
                                ..
                            } if *recorded_lane == lane.lane && *run_id == lane.run_id => {
                                serde_json::to_value(target).ok()
                            }
                            _ => None,
                        })
                        .collect::<HashSet<_>>();
                    let persisted = self
                        .session_tree
                        .nodes
                        .values()
                        .filter(|node| {
                            let mut parent = node.parent_id.as_deref();
                            while let Some(parent_id) = parent {
                                if parent_id == marker_id {
                                    return true;
                                }
                                parent = self
                                    .session_tree
                                    .nodes
                                    .get(parent_id)
                                    .and_then(|parent| parent.parent_id.as_deref());
                            }
                            false
                        })
                        .filter_map(|node| {
                            (!matches!(node.message, AgentMessage::Custom { .. }))
                                .then_some(node.message.clone())
                        })
                        .filter(|message| {
                            serde_json::to_value(message)
                                .ok()
                                .is_some_and(|message| !recorded.contains(&message))
                        })
                        .collect::<Vec<_>>();
                    journal
                        .checkpoint(&lane.lane, &lane.run_id, &persisted)
                        .map_err(&retrying)?;
                    let outcome = match status.as_str() {
                        "aborted" => OpOutcome::Aborted,
                        "failed" => OpOutcome::Failed,
                        _ => OpOutcome::Completed,
                    };
                    journal
                        .finish_subagent_lane(&lane.lane, &lane.run_id, outcome, error.clone())
                        .map_err(&retrying)?;
                    let (status, detail) = match status.as_str() {
                        "aborted" => (SubagentRecoveryStatus::Aborted, "Recovery was aborted"),
                        "failed" => (SubagentRecoveryStatus::Retrying, "Recovery needs retry"),
                        _ => (SubagentRecoveryStatus::Recovered, "Recovered prior work"),
                    };
                    let _ = self
                        .agent
                        .loop_engine
                        .event_tx
                        .send(AgentEvent::SubagentRecovery {
                            run_id: lane.run_id.clone(),
                            status,
                            detail: Some(detail.into()),
                        });
                    recovered += 1;
                    continue;
                }

                if lane.safe_tools.is_empty()
                    && lane.unsafe_tools.is_empty()
                    && lane
                        .messages
                        .iter()
                        .any(|message| matches!(message, AgentMessage::Tool { .. }))
                {
                    journal
                        .finish_subagent_lane(&lane.lane, &lane.run_id, OpOutcome::Completed, None)
                        .map_err(&retrying)?;
                    recovered += 1;
                    continue;
                }

                let claimed_safe_tools = journal
                    .claim_safe_replays(&lane.safe_tools)
                    .map_err(&retrying)?;
                let safe_results = self.replay_safe_tools(&claimed_safe_tools).await;
                let safe_messages = safe_results
                    .iter()
                    .cloned()
                    .map(|result| {
                        let terminate = result.terminates();
                        AgentMessage::Tool {
                            tool_call_id: result.tool_call_id,
                            name: result.name,
                            content: result.content,
                            is_error: result.is_error,
                            terminate,
                        }
                    })
                    .collect::<Vec<_>>();
                let unsafe_tool_ids = lane
                    .unsafe_tools
                    .iter()
                    .filter_map(|record| match record {
                        OpRecord::ToolStarted { tool_call_id, .. } => Some(tool_call_id.clone()),
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let unsafe_messages = lane
                    .messages
                    .iter()
                    .filter(|message| {
                        matches!(
                            message,
                            AgentMessage::Tool { tool_call_id, .. }
                                if unsafe_tool_ids.contains(tool_call_id)
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let mut tool_messages = unsafe_messages;
                tool_messages.extend(safe_messages.clone());
                journal
                    .checkpoint(&lane.lane, &lane.run_id, &tool_messages)
                    .map_err(&retrying)?;
                if unsafe_tool_ids.is_empty() && !safe_results.is_empty() {
                    journal.refresh().map_err(&retrying)?;
                    let results = safe_results
                        .iter()
                        .map(|result| threadlane_agent::harness::ToolResult {
                            call_id: result.tool_call_id.clone(),
                            name: result.name.clone(),
                            content: result.content.clone(),
                            is_error: result.is_error,
                            terminate: result.terminates(),
                        })
                        .collect::<Vec<_>>();
                    journal
                        .store
                        .finish_existing_tool_batch(&lane.run_id, &results, TokenUsage::default())
                        .map_err(|error| error.to_string())
                        .map_err(&retrying)?;
                    journal
                        .store
                        .drive_to_completion()
                        .map_err(|error| error.to_string())
                        .map_err(&retrying)?;
                }

                if !unsafe_tool_ids.is_empty() {
                    let error =
                        Some("Interrupted unsafe tool execution was not replayed".to_string());
                    let mut messages =
                        Vec::with_capacity(1 + lane.messages.len() + safe_messages.len());
                    messages.push(AgentMessage::Custom {
                        custom_type: "subagent_lane".into(),
                        payload: serde_json::json!({
                            "lane": lane.lane,
                            "run_id": lane.run_id,
                            "agent": "recovered",
                            "task": lane.task,
                            "status": "aborted",
                            "error": error,
                        }),
                    });
                    messages.extend(lane.messages.clone());
                    messages.extend(safe_messages);
                    self.session_tree
                        .append_passive_branch_in_memory(lane.source_leaf_id.as_deref(), messages)
                        .map_err(&retrying)?;
                    // Refresh the journal store so it sees entries written by checkpoint()
                    // before reconcile_abort_run validates the ToolFinished invariants.
                    journal.refresh().map_err(&retrying)?;
                    journal
                        .finish_subagent_lane(&lane.lane, &lane.run_id, OpOutcome::Aborted, error)
                        .map_err(&retrying)?;
                    let _ = self
                        .agent
                        .loop_engine
                        .event_tx
                        .send(AgentEvent::SubagentRecovery {
                            run_id: lane.run_id.clone(),
                            status: SubagentRecoveryStatus::Aborted,
                            detail: Some("Unsafe tool was not replayed".into()),
                        });
                    recovered += 1;
                    continue;
                }

                let mut resume_messages = lane.messages.clone();
                resume_messages.extend(safe_messages);
                let _ = self
                    .agent
                    .loop_engine
                    .event_tx
                    .send(AgentEvent::SubagentRecovery {
                        run_id: lane.run_id.clone(),
                        status: SubagentRecoveryStatus::Retrying,
                        detail: Some("Resuming interrupted task".into()),
                    });
                let model = self.agent.loop_engine.state.lock().await.model.clone();
                #[cfg(test)]
                let scheduler_observer = self
                    .subagent_work_observer
                    .lock()
                    .ok()
                    .and_then(|observer| observer.clone());
                let result = run_subagent_task(
                    AgentConfig {
                        name: "recovered".into(),
                        description: "Recovered interrupted subagent".into(),
                        tools: None,
                        model: None,
                        system_prompt:
                            "Resume the interrupted child task from its durable checkpoint.".into(),
                        source: crate::agents::AgentSource::Project,
                        file_path: self.work_dir.clone(),
                    },
                    lane.task.clone(),
                    SubagentRunContext {
                        api_key: self.agent.loop_engine.api_key.clone(),
                        account_id: self.agent.loop_engine.account_id.clone(),
                        parent_model: model,
                        parent_session_id: self.session_tree.session_id.clone(),
                        work_dir: self.work_dir.clone(),
                        extensions: self.wasi_extensions.clone(),
                        parent_event_tx: self.agent.loop_engine.event_tx.clone(),
                        parent_leaf_id: lane.source_leaf_id.clone(),
                        session_file: self.session_tree.file_path.clone(),
                        #[cfg(test)]
                        scheduler_observer,
                        #[cfg(test)]
                        child_work_observer: None,
                        #[cfg(test)]
                        child_tool_observer: None,
                        semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
                    },
                    NEXT_SUBAGENT_UI_RUN_ID.fetch_add(1, Ordering::Relaxed),
                    0,
                    SubagentLaneIdentity {
                        lane_name: lane.lane.clone(),
                        run_id: lane.run_id.clone(),
                        source_leaf_id: lane.source_leaf_id.clone(),
                        started_seq: lane.started_seq,
                    },
                    resume_messages.clone(),
                )
                .await;
                let (status, outcome, error, resumed_messages) = match result {
                    Ok(result) if result.error.is_none() => {
                        ("completed", OpOutcome::Completed, None, result.messages)
                    }
                    Ok(result) => ("failed", OpOutcome::Failed, result.error, result.messages),
                    Err(error) => ("failed", OpOutcome::Failed, Some(error), resume_messages),
                };
                let mut messages = Vec::with_capacity(1 + resumed_messages.len());
                messages.push(AgentMessage::Custom {
                    custom_type: "subagent_lane".into(),
                    payload: serde_json::json!({
                        "lane": lane.lane,
                        "run_id": lane.run_id,
                        "agent": "recovered",
                        "task": lane.task,
                        "status": status,
                        "error": error,
                    }),
                });
                messages.extend(resumed_messages);
                self.session_tree
                    .append_passive_branch_in_memory(lane.source_leaf_id.as_deref(), messages)
                    .map_err(&retrying)?;
                journal
                    .finish_subagent_lane(&lane.lane, &lane.run_id, outcome, error)
                    .map_err(&retrying)?;
                let (status, detail) = if status == "completed" {
                    (SubagentRecoveryStatus::Recovered, "Recovery complete")
                } else {
                    (SubagentRecoveryStatus::Retrying, "Recovery needs retry")
                };
                let _ = self
                    .agent
                    .loop_engine
                    .event_tx
                    .send(AgentEvent::SubagentRecovery {
                        run_id: lane.run_id.clone(),
                        status,
                        detail: Some(detail.into()),
                    });
                recovered += 1;
            }
            Ok(recovered)
        }
        .await;

        if result.is_ok() {
            self.interrupted_subagent_recovery = InterruptedSubagentRecoveryState::Complete;
        }
        result
    }

    async fn repair_interrupted_history(&mut self) -> bool {
        if let Some(path) = self.session_tree.file_path.clone() {
            if self.harness_journal.is_some() {
                let Ok(tree) = SessionTree::load_from_file(&path) else {
                    return false;
                };
                let branch = tree.get_active_branch_messages();
                let mut state = self.agent.loop_engine.state.lock().await;
                let mut messages = Vec::with_capacity(branch.len() + 1);
                messages.push(AgentMessage::System {
                    content: state.system_prompt.clone(),
                });
                messages.extend(
                    branch
                        .into_iter()
                        .filter(|message| !matches!(message, AgentMessage::System { .. })),
                );
                let changed = state.messages != messages;
                self.session_tree = tree;
                state.messages = messages;
                return changed;
            }
        }
        let repaired_branch = {
            let mut state = self.agent.loop_engine.state.lock().await;
            if !repair_interrupted_tool_turn(&mut state.messages) {
                return false;
            }
            state
                .messages
                .iter()
                .filter(|message| !matches!(message, AgentMessage::System { .. }))
                .cloned()
                .collect::<Vec<_>>()
        };

        let persisted_branch = self.session_tree.get_active_branch_messages();
        if serde_json::to_value(&persisted_branch).ok()
            != serde_json::to_value(&repaired_branch).ok()
        {
            if self.harness_journal.is_some() {
                self.session_tree
                    .replace_active_branch_in_memory(repaired_branch);
            } else {
                self.session_tree.replace_active_branch(repaired_branch);
            }
        }
        true
    }

    pub async fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.agent.set_reasoning_effort(effort).await;
    }

    pub async fn set_model(&mut self, model: String) -> Result<(), String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("model cannot be empty".into());
        }
        if self.harness_journal.is_none() {
            self.session_tree
                .set_model(model.to_string())
                .map_err(|error| format!("Could not persist model switch: {error}"))?;
        }
        if let Some(journal) = self.harness_journal.as_mut() {
            journal.refresh()?;
            journal
                .store
                .set_fact("main", "model", model.to_string(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            self.session_tree
                .set_model_in_memory(model.to_string())
                .map_err(|error| format!("Could not update model switch: {error}"))?;
        }
        self.agent.loop_engine.state.lock().await.model = model.to_string();
        Ok(())
    }

    pub fn set_name(&mut self, name: String) -> Result<(), String> {
        if self.harness_journal.is_some() {
            let journal = self
                .harness_journal
                .as_mut()
                .ok_or_else(|| "harness journal disappeared during name update".to_string())?;
            journal.refresh().map_err(|error| error.to_string())?;
            journal
                .store
                .set_fact("main", "name", name.clone(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            self.session_tree
                .set_name_in_memory(name)
                .map_err(|error| format!("Could not update session name: {error}"))?;
            Ok(())
        } else {
            self.session_tree
                .set_name(name)
                .map_err(|error| format!("Could not persist session name: {error}"))
        }
    }

    pub fn set_fact(&mut self, key: &str, value: &str) -> Result<(), String> {
        if self.harness_journal.is_some() {
            let journal = self
                .harness_journal
                .as_mut()
                .ok_or_else(|| "harness journal disappeared during fact update".to_string())?;
            journal.refresh().map_err(|error| error.to_string())?;
            journal
                .store
                .set_fact("main", key, value.to_string(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            self.session_tree.set_fact_in_memory(key, value);
            Ok(())
        } else {
            self.session_tree
                .set_fact(key.to_owned(), value.to_owned())
                .map_err(|error| format!("Could not persist session fact: {error}"))
        }
    }

    pub async fn available_models(&self) -> Vec<String> {
        let api_key = self.agent.loop_engine.api_key.clone();
        let account_id = self.agent.loop_engine.account_id.clone();
        fetch_available_models(&api_key, account_id.as_deref()).await
    }

    async fn recover_harness_tool_batch(&mut self, run_id: &str) -> Result<bool, String> {
        let (assistant_entry_id, specs) = {
            let journal = self
                .harness_journal
                .as_mut()
                .ok_or_else(|| "harness journal is unavailable".to_string())?;
            journal.refresh()?;
            let state =
                Reducer::reduce(journal.store.store()).map_err(|error| error.to_string())?;
            let lane = state
                .lane("main")
                .ok_or_else(|| "main harness lane is unavailable".to_string())?;
            let unfinished = lane
                .tools
                .iter()
                .filter(|tool| tool.run_id == run_id && !tool.completed)
                .cloned()
                .collect::<Vec<_>>();
            let Some(first) = unfinished.first() else {
                return Ok(false);
            };
            let assistant_entry_id = first.assistant_entry_id.clone();
            let assistant = journal
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == assistant_entry_id)
                .ok_or_else(|| "unfinished tool batch assistant entry is missing".to_string())?;
            let calls = match &assistant.message {
                AgentMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } => calls,
                _ => return Err("unfinished tool batch has no assistant tool calls".into()),
            };
            let mut specs = Vec::with_capacity(unfinished.len());
            for tool in &unfinished {
                let call = calls.get(tool.tool_index).ok_or_else(|| {
                    "unfinished tool ordinal is outside assistant declaration".to_string()
                })?;
                let effective_args = journal
                    .store
                    .records()
                    .iter()
                    .find_map(|record| match record {
                        HarnessRecord::ToolStarted {
                            run_id: record_run_id,
                            tool_call_id,
                            effective_args,
                            ..
                        } if record_run_id == run_id && tool_call_id == &tool.tool_call_id => {
                            Some(effective_args.clone())
                        }
                        _ => None,
                    })
                    .ok_or_else(|| "unfinished tool intent arguments are missing".to_string())?;
                specs.push(ToolSpec {
                    index: tool.tool_index,
                    call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    effective_args,
                    result_entry_id: tool.result_entry_id.clone(),
                    replay: tool.replay.clone(),
                });
            }
            (assistant_entry_id, specs)
        };

        let recoveries = {
            let journal = self
                .harness_journal
                .as_mut()
                .ok_or_else(|| "harness journal is unavailable".to_string())?;
            let recoveries = journal
                .store
                .resume_tool_batch(run_id, &assistant_entry_id, &specs)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            recoveries
        };

        let replay_specs = recoveries
            .iter()
            .filter_map(|recovery| match recovery {
                ToolRecovery::Replay(spec) => Some(spec.clone()),
                ToolRecovery::Synthesized(_) => None,
            })
            .collect::<Vec<_>>();
        let replay_calls = replay_specs
            .iter()
            .map(|spec| threadlane_provider::openai::ToolCall {
                id: spec.call_id.clone(),
                r#type: "function".into(),
                function: threadlane_provider::openai::ToolCallFunction {
                    name: spec.name.clone(),
                    arguments: spec.effective_args.to_string(),
                },
                thought_signature: None,
            })
            .collect::<Vec<_>>();
        let replay_results = if replay_calls.is_empty() {
            Vec::new()
        } else {
            self.agent
                .loop_engine
                .execute_tools_for_replay(&replay_calls)
                .await
        };

        let mut messages = Vec::with_capacity(recoveries.len());
        for recovery in recoveries {
            let (spec, result) = match recovery {
                ToolRecovery::Replay(spec) => {
                    let result = replay_results
                        .iter()
                        .find(|result| result.tool_call_id == spec.call_id)
                        .cloned()
                        .ok_or_else(|| {
                            format!("safe replay produced no result for {}", spec.call_id)
                        })?;
                    (spec, result)
                }
                ToolRecovery::Synthesized(result) => {
                    let spec = specs
                        .iter()
                        .find(|spec| spec.call_id == result.call_id)
                        .cloned()
                        .ok_or_else(|| {
                            format!("synthesized result has no spec for {}", result.call_id)
                        })?;
                    (
                        spec,
                        AgentToolResult::external(
                            result.call_id.clone(),
                            result.name.clone(),
                            result.content.clone(),
                            result.is_error,
                        ),
                    )
                }
            };
            if replay_specs
                .iter()
                .any(|replay| replay.call_id == spec.call_id)
            {
                let journal = self
                    .harness_journal
                    .as_mut()
                    .ok_or_else(|| "harness journal is unavailable".to_string())?;
                journal.append_replayed_tool_entry(run_id, &assistant_entry_id, &spec, &result)?;
                journal.finish_replayed_tool(run_id, &result)?;
                journal.run_after_tool_hook(run_id);
            }
            let terminate = result.terminates();
            messages.push(AgentMessage::Tool {
                tool_call_id: result.tool_call_id,
                name: result.name,
                content: result.content,
                is_error: result.is_error,
                terminate,
            });
        }

        {
            let mut state = self.agent.loop_engine.state.lock().await;
            for message in messages {
                if !state.messages.iter().any(|current| current == &message) {
                    state.messages.push(message);
                }
            }
        }
        if let Some(path) = self.session_tree.file_path.clone() {
            self.session_tree = SessionTree::load_from_file(&path)
                .map_err(|error| format!("failed to refresh recovered tool history: {error}"))?;
        }
        Ok(true)
    }

    pub(crate) async fn handle_input(&mut self, input: &str) -> Option<Result<String, String>> {
        self.handle_input_with_images(input, Vec::new()).await
    }

    pub async fn resume_suspended_harness(&mut self) -> Result<bool, String> {
        if let Some(error) = self.harness_journal_error.as_ref() {
            return Err(format!("Harness Error: {error}"));
        }
        let Some(journal) = self.harness_journal.as_mut() else {
            return Ok(false);
        };
        journal.refresh()?;
        journal
            .store
            .restore_hooks_for_lane("main")
            .map_err(|error| error.to_string())?;
        let state = Reducer::reduce(&journal.store).map_err(|error| error.to_string())?;
        let Some(lane) = state.lane("main") else {
            return Ok(false);
        };
        let Some(run_id) = lane.open_operation.clone() else {
            return Ok(false);
        };
        let context = HookContext {
            session_id: journal.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.clone()),
            resume_data: None,
        };
        for failure in journal.store.hooks().run_before_resume(&context) {
            eprintln!(
                "before-resume hook {} failed: {}",
                failure.id, failure.message
            );
        }
        if lane.abort_requested {
            journal.recover_abort()?;
            return Ok(true);
        }
        if lane.retry.is_some() {
            journal.begin_retry(&run_id)?;
            journal.refresh()?;
        }
        let start_seq = journal
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == &run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        if journal.store.entries().iter().any(|entry| {
            entry.seq > start_seq
                && matches!(
                    &entry.message,
                    AgentMessage::Assistant {
                        deferred_handle: Some(_),
                        ..
                    }
                )
        }) {
            return self.redeem_suspended_deferred_from_provider(&run_id).await;
        }
        self.recover_harness_tool_batch(&run_id).await?;
        let journal = self
            .harness_journal
            .as_mut()
            .ok_or_else(|| "harness journal disappeared during tool recovery".to_string())?;
        journal.refresh()?;
        let has_attempt = journal.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::StepAttempt { run_id: record_run_id, .. } if record_run_id == &run_id)
        });
        let has_terminal_assistant = journal.store.entries().iter().any(|entry| {
            entry.seq > start_seq
                && matches!(
                    &entry.message,
                    AgentMessage::Assistant {
                        tool_calls: None,
                        ..
                    }
                )
        });
        if has_terminal_assistant && !has_attempt {
            journal.record_assistant_attempt(&run_id, TokenUsage::default())?;
            journal.finish(&run_id, OpOutcome::Completed, None)?;
            return Ok(true);
        }
        *self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())? = Some(run_id.clone());
        let mut events = self.subscribe();
        self.agent.resume_pending_turn().await;
        self.sync_session_tree_and_dispatch_assistant_hooks().await;
        self.run_scheduled_agent_work().await;
        let mut usage = TokenUsage::default();
        let mut failure = None;
        let mut tool_termination = HashMap::new();
        while let Ok(event) = events.try_recv() {
            match event {
                AgentEvent::AgentEnd { usage: value } => usage = value,
                AgentEvent::AgentError { error } => failure = Some(error),
                AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    result,
                    ..
                } => {
                    tool_termination.insert(tool_call_id, result.terminates());
                }
                _ => {}
            }
        }
        let journal = self
            .harness_journal
            .as_mut()
            .ok_or_else(|| "harness journal disappeared during resume".to_string())?;
        if let Some(error) = failure {
            if is_retryable_generation_error(&error)
                && journal.schedule_retry(&run_id, &error).is_ok()
            {
                return Err(error);
            }
            journal.finish(&run_id, OpOutcome::Failed, Some(error.clone()))?;
            if let Ok(mut active) = self.harness_run_id.lock() {
                if active.as_deref() == Some(run_id.as_str()) {
                    *active = None;
                }
            }
            return Err(error);
        }
        journal.record_completed_tools_with_termination(&run_id, &tool_termination)?;
        journal.record_assistant_attempt(&run_id, usage)?;
        journal.finish(&run_id, OpOutcome::Completed, None)?;
        if let Ok(mut active) = self.harness_run_id.lock() {
            if active.as_deref() == Some(run_id.as_str()) {
                *active = None;
            }
        }
        Ok(true)
    }

    pub fn redeem_suspended_deferred(
        &mut self,
        run_id: &str,
        resolution: DeferredResolution,
    ) -> Result<bool, String> {
        let journal = self
            .harness_journal
            .as_mut()
            .ok_or_else(|| "harness journal is unavailable".to_string())?;
        journal.redeem_deferred(run_id, resolution)
    }

    pub async fn redeem_suspended_deferred_from_provider(
        &mut self,
        run_id: &str,
    ) -> Result<bool, String> {
        let handle = {
            let journal = self
                .harness_journal
                .as_mut()
                .ok_or_else(|| "harness journal is unavailable".to_string())?;
            journal.refresh()?;
            journal
                .store
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match &entry.message {
                    AgentMessage::Assistant {
                        deferred_handle: Some(handle),
                        ..
                    } => Some(handle.clone()),
                    _ => None,
                })
                .ok_or_else(|| format!("deferred handle for {run_id} is missing"))?
        };
        let resolution = match self
            .agent
            .fetch_deferred(&handle.model, &handle.handle_id)
            .await?
        {
            threadlane_provider::DeferredResponse::Pending => DeferredResolution::Pending(handle),
            threadlane_provider::DeferredResponse::Ready { content } => {
                DeferredResolution::Ready(AgentMessage::Assistant {
                    content: Some(content),
                    tool_calls: None,
                    stop_reason: Some("deferred_ready".into()),
                    deferred_handle: None,
                })
            }
            threadlane_provider::DeferredResponse::Error { message } => {
                DeferredResolution::Error(message)
            }
        };
        self.redeem_suspended_deferred(run_id, resolution)
    }

    pub async fn cancel_suspended_deferred(&mut self, run_id: &str) -> Result<(), String> {
        let handle = {
            let journal = self
                .harness_journal
                .as_mut()
                .ok_or_else(|| "harness journal is unavailable".to_string())?;
            journal.refresh()?;
            let open_run = Reducer::reduce(&journal.store)
                .map_err(|error| error.to_string())?
                .lane("main")
                .and_then(|lane| lane.open_operation.clone());
            if open_run.as_deref() != Some(run_id) {
                return Err(format!("deferred operation {run_id} is not open"));
            }
            journal
                .request_abort()?
                .ok_or_else(|| format!("deferred operation {run_id} is not open"))?;
            journal
                .store
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match &entry.message {
                    AgentMessage::Assistant {
                        deferred_handle: Some(handle),
                        ..
                    } => Some(handle.clone()),
                    _ => None,
                })
                .ok_or_else(|| format!("deferred handle for {run_id} is missing"))?
        };
        self.agent
            .cancel_deferred(&handle.model, &handle.handle_id)
            .await
            .or_else(|error| {
                eprintln!("Deferred cancellation failed after durable abort: {error}");
                Ok(())
            })
    }

    pub async fn handle_input_with_images(
        &mut self,
        input: &str,
        images: Vec<ImageAttachment>,
    ) -> Option<Result<String, String>> {
        self.cancellation.clear_cancellation_guard();
        if let Err(error) = self.recover_interrupted_subagent_lanes().await {
            return Some(Err(error));
        }
        if let Some(error) = self.harness_journal_error.as_ref() {
            let error = format!("Harness Error: {error}");
            let _ = self
                .agent
                .loop_engine
                .event_tx
                .send(AgentEvent::AgentError {
                    error: error.clone(),
                });
            return Some(Err(error));
        }
        let adopted_harness_run = self
            .harness_run_id
            .lock()
            .ok()
            .is_some_and(|run_id| run_id.is_some());
        if !adopted_harness_run {
            if let Some(journal) = self.harness_journal.as_mut() {
                match journal.recover_abort() {
                    Ok(_) => {}
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                }
            }
        }
        self.repair_interrupted_history().await;
        *self.dispatch_parent_leaf.lock().unwrap() =
            self.session_tree.active_node_id().map(str::to_owned);
        let trimmed = input.trim();

        // 1. Expand prompt templates (e.g. /review, /component Button) if match
        if self.prompt_templates.is_none() {
            let global_dir = std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".threadlane"))
                .unwrap_or_else(|| self.work_dir.join(".threadlane"));
            self.prompt_templates = Some(crate::prompt_templates::load_prompt_templates(
                &self.work_dir,
                &global_dir,
            ));
        }
        let templates = self.prompt_templates.as_ref().unwrap();
        let expanded_input = crate::prompt_templates::expand_prompt_template(trimmed, templates);
        let effective_input = expanded_input.trim();

        if let Some(command_input) = effective_input.strip_prefix('/') {
            let mut parts = command_input.split_whitespace();
            let cmd_name = parts.next().unwrap_or("");
            let cmd_args = parts.collect::<Vec<&str>>().join(" ");

            if cmd_name.starts_with("skill:") || cmd_name == "skill" {
                let skill_name = if let Some(skill_name) = cmd_name.strip_prefix("skill:") {
                    skill_name
                } else {
                    cmd_args.trim()
                };

                match self.skills.get_skill_instructions(skill_name) {
                    Ok(instructions) => {
                        let prompt = format!(
                            "Use the following Skill instructions for '{}':\n\n{}",
                            skill_name, instructions
                        );
                        let visible_prompt = AgentMessage::user(input, images.clone());
                        let harness_run_id = match self.begin_harness_run(visible_prompt) {
                            Ok(run_id) => run_id,
                            Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                        };
                        let parent_leaf = if harness_run_id.is_some() {
                            self.session_tree
                                .add_message_in_memory(AgentMessage::user(input, images.clone()))
                        } else {
                            self.session_tree
                                .add_message(AgentMessage::user(input, images.clone()))
                        };
                        *self.dispatch_parent_leaf.lock().unwrap() = Some(parent_leaf);
                        self.agent
                            .prompt_message(AgentMessage::user(prompt, images.clone()))
                            .await;
                        self.sync_session_tree_and_dispatch_assistant_hooks().await;
                        self.run_scheduled_agent_work().await;
                        if let Err(error) = self.commit_completed_subagent_lanes() {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            let _ = self.finish_harness_run(
                                harness_run_id.as_deref(),
                                OpOutcome::Failed,
                                Some(error.clone()),
                            );
                            return Some(Err(error));
                        }
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        if let Err(error) = self.finish_harness_run(
                            harness_run_id.as_deref(),
                            OpOutcome::Completed,
                            None,
                        ) {
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                        return Some(Ok(format!("Loaded skill '{}'", skill_name)));
                    }
                    Err(err) => return Some(Err(format!("Skill Error: {}", err))),
                }
            }

            if cmd_name == "subagent" {
                let task_prompt = cmd_args.trim();
                if task_prompt.is_empty() {
                    return Some(Err("Usage: /subagent <task description>".to_string()));
                }
                let task = AgentRunTask {
                    agent: "worker".to_string(),
                    task: task_prompt.to_string(),
                    instructions: None,
                    tools: None,
                    model: None,
                };
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt) {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                if let Some(run_id) = harness_run_id.as_deref() {
                    if let Some(journal) = self.harness_journal.as_mut() {
                        if let Err(error) = journal.prepare_assistant_attempt(run_id) {
                            let _ = self.finish_harness_run(
                                Some(run_id),
                                OpOutcome::Failed,
                                Some(error.clone()),
                            );
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                    }
                }
                let parent_leaf = if harness_run_id.is_some() {
                    self.session_tree
                        .add_message_in_memory(AgentMessage::user(input, images.clone()))
                } else {
                    self.session_tree
                        .add_message(AgentMessage::user(input, images.clone()))
                };
                *self.dispatch_parent_leaf.lock().unwrap() = Some(parent_leaf);
                let result = match (self.agent_runner)(vec![task], false, None).await {
                    Ok(result) => result,
                    Err(err) => {
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        let _ = self.finish_harness_run(
                            harness_run_id.as_deref(),
                            OpOutcome::Failed,
                            Some(err.clone()),
                        );
                        return Some(Err(format!("Subagent Error: {err}")));
                    }
                };
                let output = result["output"].as_str().unwrap_or_default().to_string();
                if let Err(error) = self.commit_completed_subagent_lanes() {
                    *self.dispatch_parent_leaf.lock().unwrap() = None;
                    let _ = self.finish_harness_run(
                        harness_run_id.as_deref(),
                        OpOutcome::Failed,
                        Some(error.clone()),
                    );
                    return Some(Err(error));
                }
                *self.dispatch_parent_leaf.lock().unwrap() = None;
                let assistant = AgentMessage::Assistant {
                    content: Some(output.clone()),
                    tool_calls: None,
                    stop_reason: Some("subagent".into()),
                    deferred_handle: None,
                };
                if let Some(run_id) = harness_run_id.as_deref() {
                    if let Some(journal) = self.harness_journal.as_mut() {
                        if let Err(error) =
                            journal.append_message(assistant.clone()).and_then(|_| {
                                journal.record_assistant_attempt(run_id, TokenUsage::default())
                            })
                        {
                            let _ = self.finish_harness_run(
                                Some(run_id),
                                OpOutcome::Failed,
                                Some(error.clone()),
                            );
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                    }
                }
                if harness_run_id.is_some() {
                    self.session_tree.add_message_in_memory(assistant);
                } else {
                    self.session_tree.add_message(assistant);
                }
                self.run_scheduled_agent_work().await;
                if let Err(error) =
                    self.finish_harness_run(harness_run_id.as_deref(), OpOutcome::Completed, None)
                {
                    return Some(Err(format!("Harness Error: {error}")));
                }
                return Some(Ok(output));
            }

            if let Some(res) = self
                .wasi_extensions
                .execute_command_with_effects(cmd_name, &cmd_args)
            {
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt) {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                let parent_leaf = if harness_run_id.is_some() {
                    self.session_tree
                        .add_message_in_memory(AgentMessage::user(input, images.clone()))
                } else {
                    self.session_tree
                        .add_message(AgentMessage::user(input, images.clone()))
                };
                *self.dispatch_parent_leaf.lock().unwrap() = Some(parent_leaf);
                return match res {
                    Ok(result) => {
                        let message = if result.message.is_empty() {
                            None
                        } else {
                            Some(result.message)
                        };
                        let dispatch = match self
                            .broker_dispatcher
                            .dispatch_envelopes(result.host_broker_requests)
                            .await
                        {
                            Ok(dispatch) => dispatch,
                            Err(error) => {
                                let _ = self.finish_harness_run(
                                    harness_run_id.as_deref(),
                                    OpOutcome::Failed,
                                    Some(error.message.clone()),
                                );
                                return Some(Err(format!("WASI Broker Error: {}", error.message)));
                            }
                        };
                        let agent_run_output =
                            dispatch.operation_results.iter().find_map(|result| {
                                if result.request.capability != "agent"
                                    || result.request.operation != "run"
                                {
                                    return None;
                                }
                                if let Some(error) = &result.error {
                                    return Some(Err(format!(
                                        "WASI Broker Error: {}",
                                        error.message
                                    )));
                                }
                                let output = result.value["output"].as_str().ok_or_else(|| {
                                    "agent.run returned no formatted output".to_string()
                                });
                                let thinking = serde_json::from_value::<Vec<AgentMessage>>(
                                    result.value["thinking"].clone(),
                                )
                                .map_err(|error| {
                                    format!("agent.run returned invalid thinking: {error}")
                                });
                                match (output, thinking) {
                                    (Ok(output), Ok(thinking)) => {
                                        for message in thinking {
                                            if let Err(error) = self.append_command_message(message)
                                            {
                                                return Some(Err(error));
                                            }
                                        }
                                        if let Err(error) =
                                            self.append_command_message(AgentMessage::Assistant {
                                                content: Some(output.to_string()),
                                                tool_calls: None,
                                                stop_reason: None,
                                                deferred_handle: None,
                                            })
                                        {
                                            return Some(Err(error));
                                        }
                                        Some(Ok(output.to_string()))
                                    }
                                    (Err(error), _) | (_, Err(error)) => Some(Err(error)),
                                }
                            });
                        self.wasi_extensions
                            .enqueue_broker_results(dispatch.operation_results);
                        self.run_scheduled_agent_work().await;
                        if result.api_version == 1 {
                            for effect in result.effects {
                                match effect {
                                    WasiLegacyEffect::SetToolPolicy { policy } => {
                                        let mut pol = self.tool_policy.lock().await;
                                        match policy.as_str() {
                                            "read_only" => *pol = ToolPolicy::ReadOnly,
                                            "full" => *pol = ToolPolicy::FullAccess,
                                            _ => continue,
                                        }
                                    }
                                    WasiLegacyEffect::RequestModelTurn { prompt } => {
                                        self.agent.prompt(&prompt).await;
                                        self.sync_session_tree_and_dispatch_assistant_hooks().await;
                                    }
                                }
                            }
                        }
                        if let Err(error) = self.commit_completed_subagent_lanes() {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            let _ = self.finish_harness_run(
                                harness_run_id.as_deref(),
                                OpOutcome::Failed,
                                Some(error.clone()),
                            );
                            return Some(Err(error));
                        }
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        if let Some(agent_run_output) = agent_run_output {
                            let result = agent_run_output;
                            let outcome = if result.is_ok() {
                                OpOutcome::Completed
                            } else {
                                OpOutcome::Failed
                            };
                            if let Err(error) = self.finish_harness_run(
                                harness_run_id.as_deref(),
                                outcome,
                                result.as_ref().err().cloned(),
                            ) {
                                return Some(Err(format!("Harness Error: {error}")));
                            }
                            return Some(result);
                        }
                        let result = message.map(Ok);
                        let outcome = if result.is_some() {
                            OpOutcome::Completed
                        } else {
                            OpOutcome::Failed
                        };
                        if let Err(error) =
                            self.finish_harness_run(harness_run_id.as_deref(), outcome, None)
                        {
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                        result
                    }
                    Err(err) => {
                        let message = format!("WASI Extension Error: {err}");
                        let _ = self.finish_harness_run(
                            harness_run_id.as_deref(),
                            OpOutcome::Failed,
                            Some(message.clone()),
                        );
                        Some(Err(message))
                    }
                };
            }

            if let Some(cmd_action) = parse_slash_command(effective_input) {
                if cmd_action == CommandAction::Quit {
                    return Some(Ok("quitting".to_string()));
                }
                if cmd_action == CommandAction::Compact {
                    return Some(match self.compact_history_with_harness().await {
                        Ok(true) => Ok("Context compacted in the current session.".into()),
                        Ok(false) => Ok("Nothing to compact yet.".into()),
                        Err(error) => Err(format!("Harness Error: {error}")),
                    });
                }
                if let CommandAction::SwitchTreeBranch(node_id) = &cmd_action {
                    return Some(self.navigate_tree_branch(node_id).await);
                }
                if let CommandAction::SwitchModel(model) = &cmd_action {
                    if !model.is_empty() {
                        return Some(
                            self.set_model(model.clone())
                                .await
                                .map(|_| format!("Switched model to: {model}")),
                        );
                    }
                }
                if let CommandAction::SetName(name) = &cmd_action {
                    return Some(
                        self.set_name(name.clone())
                            .map(|_| format!("Session name set to: {name}")),
                    );
                }
                let output =
                    execute_slash_command(cmd_action, &mut self.agent, &mut self.session_tree)
                        .await;
                return Some(Ok(output));
            }
        }

        if self.agent.auto_compact_history().await {
            let state = self.agent.get_state().await;
            if let Some(summary) = state
                .messages
                .iter()
                .rev()
                .find_map(threadlane_agent::compaction_summary_text)
            {
                let retained_tail = compaction_retained_tail(&state.messages);
                if let Err(error) = self.persist_harness_compaction(summary, &retained_tail) {
                    let _ = self
                        .agent
                        .loop_engine
                        .event_tx
                        .send(AgentEvent::AgentError { error });
                    return None;
                }
            }
            if self.harness_journal.is_some() {
                let path = self
                    .session_tree
                    .file_path
                    .clone()
                    .ok_or_else(|| "harness compaction has no session path".to_string());
                match path.and_then(|path| {
                    SessionTree::load_from_file(&path)
                        .map_err(|error| format!("failed to reload compacted session: {error}"))
                }) {
                    Ok(tree) => self.session_tree = tree,
                    Err(error) => {
                        let _ = self
                            .agent
                            .loop_engine
                            .event_tx
                            .send(AgentEvent::AgentError { error });
                        return None;
                    }
                }
            } else {
                self.session_tree.replace_active_branch(state.messages);
            }
        }

        let msg = AgentMessage::user(effective_input, images);
        let harness_run_id = match self.begin_harness_run(msg.clone()) {
            Ok(run_id) => run_id,
            Err(error) => {
                let message = format!("Harness Error: {error}");
                let _ = self
                    .agent
                    .loop_engine
                    .event_tx
                    .send(AgentEvent::AgentError {
                        error: message.clone(),
                    });
                return Some(Err(message));
            }
        };
        let parent_leaf = if self
            .session_tree
            .get_active_branch_messages()
            .last()
            .is_some_and(|message| message == &msg)
        {
            self.session_tree
                .active_node_id()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if harness_run_id.is_some() {
                        self.session_tree.add_message_in_memory(msg.clone())
                    } else {
                        self.session_tree.add_message(msg.clone())
                    }
                })
        } else {
            if harness_run_id.is_some() {
                self.session_tree.add_message_in_memory(msg.clone())
            } else {
                self.session_tree.add_message(msg.clone())
            }
        };
        *self.dispatch_parent_leaf.lock().unwrap() = Some(parent_leaf);
        let mut harness_events = self.subscribe();
        self.agent.prompt_message(msg).await;
        self.sync_session_tree_and_dispatch_assistant_hooks().await;
        if let Some(error) = self.harness_journal_error.clone() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self.finish_harness_run(
                harness_run_id.as_deref(),
                OpOutcome::Failed,
                Some(error.clone()),
            );
            return Some(Err(format!("Harness Error: {error}")));
        }
        self.run_scheduled_agent_work().await;
        if let Some(error) = self.harness_journal_error.clone() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self.finish_harness_run(
                harness_run_id.as_deref(),
                OpOutcome::Failed,
                Some(error.clone()),
            );
            return Some(Err(format!("Harness Error: {error}")));
        }
        if let Err(error) = self.commit_completed_subagent_lanes() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self.finish_harness_run(
                harness_run_id.as_deref(),
                OpOutcome::Failed,
                Some(error.clone()),
            );
            let _ = self
                .agent
                .loop_engine
                .event_tx
                .send(AgentEvent::AgentError {
                    error: error.clone(),
                });
            return Some(Err(error));
        }
        *self.dispatch_parent_leaf.lock().unwrap() = None;
        let mut tool_termination = HashMap::new();
        let (usage, failure) = loop {
            match harness_events.try_recv() {
                Ok(AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    result,
                    ..
                }) => {
                    tool_termination.insert(tool_call_id, result.terminates());
                }
                Ok(AgentEvent::AgentEnd { usage }) => break (usage, None),
                Ok(AgentEvent::AgentError { error }) => break (TokenUsage::default(), Some(error)),
                Ok(_) => continue,
                Err(error) => {
                    if let Some(message) = generation_event_drain_error(error) {
                        break (TokenUsage::default(), Some(message.into()));
                    }
                }
            }
        };
        if let Some(error) = failure {
            if let Some(run_id) = harness_run_id.as_deref() {
                let completion = self.harness_journal.as_mut().map(|journal| {
                    journal.record_completed_tools_with_termination(run_id, &tool_termination)
                });
                if let Some(Err(completion_error)) = completion {
                    let _ = self.finish_harness_run(
                        Some(run_id),
                        OpOutcome::Failed,
                        Some(completion_error.clone()),
                    );
                    return Some(Err(format!("Harness Error: {completion_error}")));
                }
                if is_retryable_generation_error(&error) {
                    let scheduled = self
                        .harness_journal
                        .as_mut()
                        .map(|journal| journal.schedule_retry(run_id, &error));
                    if matches!(scheduled, Some(Ok(_))) {
                        return Some(Err(error));
                    }
                }
                let _ =
                    self.finish_harness_run(Some(run_id), OpOutcome::Failed, Some(error.clone()));
            }
            return Some(Err(error));
        }
        if let Some(run_id) = harness_run_id.as_deref() {
            let attempt_result = self.harness_journal.as_mut().map(|journal| {
                journal
                    .record_completed_tools_with_termination(run_id, &tool_termination)
                    .and_then(|_| journal.record_assistant_attempt(run_id, usage))
            });
            if let Some(Err(error)) = attempt_result {
                let _ =
                    self.finish_harness_run(Some(run_id), OpOutcome::Failed, Some(error.clone()));
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        if let Err(error) =
            self.finish_harness_run(harness_run_id.as_deref(), OpOutcome::Completed, None)
        {
            return Some(Err(format!("Harness Error: {error}")));
        }

        None
    }
}

fn compaction_retained_tail(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    let Some(summary_index) = messages
        .iter()
        .rposition(|message| threadlane_agent::compaction_summary_text(message).is_some())
    else {
        return Vec::new();
    };
    messages
        .iter()
        .skip(summary_index + 1)
        .filter(|message| !matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect()
}

async fn run_subagents_with_context(
    tasks: Vec<AgentRunTask>,
    parallel: bool,
    tool_call_id: Option<String>,
    context: SubagentRunContext,
) -> Result<(String, Vec<AgentMessage>, Vec<CompletedSubagentLane>), String> {
    let run_id = NEXT_SUBAGENT_UI_RUN_ID.fetch_add(1, Ordering::Relaxed);
    for (task_index, task) in tasks.iter().enumerate() {
        let _ = context.parent_event_tx.send(AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent: task.agent.clone(),
            task: task.task.clone(),
        });
    }
    let candidates = discover_agents(&context.work_dir, AgentScope::Both).agents;
    let lane_key = tool_call_id
        .map(|id| format!("tool-{id}"))
        .unwrap_or_else(|| "explicit".into());
    let run_one = |task_index: usize, task: AgentRunTask| {
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.name == task.agent)
            .cloned();

        let mut config = match candidate {
            Some(static_config) => static_config,
            None => {
                let sys_prompt = task.instructions.clone().unwrap_or_else(|| {
                    format!(
                        "You are a specialized subagent acting as '{}'. Complete only the assigned task and report results clearly to the parent agent.",
                        task.agent
                    )
                });
                AgentConfig {
                    name: task.agent.clone(),
                    description: format!("Dynamic subagent for {}", task.agent),
                    tools: task.tools.clone(),
                    model: task.model.clone(),
                    system_prompt: sys_prompt,
                    source: crate::agents::AgentSource::Project,
                    file_path: context.work_dir.clone(),
                }
            }
        };
        if let Some(inst) = &task.instructions {
            config.system_prompt = inst.clone();
        }
        if let Some(t) = &task.tools {
            config.tools = Some(t.clone());
        }
        if let Some(m) = &task.model {
            config.model = Some(m.clone());
        }

        let context = context.clone();
        let event_tx = context.parent_event_tx.clone();
        let lane_task = task.task.clone();
        let lane_agent = task.agent.clone();
        let lane_key = lane_key.clone();
        async move {
            let parent_leaf_id = context.parent_leaf_id.clone();
            let lane_hint = format!(
                "subagent-{}-{}:{task_index}",
                context.parent_session_id, lane_key
            );
            let _permit = context
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| "Subagent concurrency limiter closed".to_string())?;
            let start = match context.session_file.as_deref() {
                Some(path) => {
                    let mut journal = HarnessJournal::open(path)?;
                    journal.start_subagent_lane(&lane_hint, &task.task, parent_leaf_id.as_deref())
                }
                None => Ok(SubagentLaneIdentity {
                    lane_name: lane_hint.clone(),
                    run_id: lane_hint.clone(),
                    source_leaf_id: parent_leaf_id.clone(),
                    started_seq: 0,
                }),
            };
            let result = match start {
                Ok(identity) => {
                    let _ = event_tx.send(AgentEvent::SubagentStarted {
                        run_id,
                        task_index,
                        journal_run_id: identity.run_id.clone(),
                    });
                    #[cfg(test)]
                    if let Some(observer) = context.child_work_observer.as_ref() {
                        observer();
                    }
                    timeout(
                        SUBAGENT_TIMEOUT,
                        run_subagent_task(
                            config,
                            task.task,
                            context,
                            run_id,
                            task_index,
                            identity.clone(),
                            Vec::new(),
                        ),
                    )
                    .await
                    .map_err(|_| "Subagent timed out".to_string())
                    .map(|result| (result, identity))?
                }
                Err(SubagentStartError { identity, error }) => (
                    Err(error),
                    identity.unwrap_or_else(|| SubagentLaneIdentity {
                        lane_name: lane_hint.clone(),
                        run_id: lane_hint,
                        source_leaf_id: parent_leaf_id.clone(),
                        started_seq: 0,
                    }),
                ),
            };
            let (result, identity) = result;
            let (succeeded, error) = match &result {
                Ok(result) if result.error.is_none() => (true, None),
                Ok(result) => (false, result.error.clone()),
                Err(error) => (false, Some(error.clone())),
            };
            let _ = event_tx.send(AgentEvent::SubagentFinished {
                run_id,
                task_index,
                journal_run_id: identity.run_id.clone(),
                succeeded,
                error,
            });
            let lane = CompletedSubagentLane {
                lane_name: identity.lane_name,
                run_id: identity.run_id,
                parent_leaf_id: identity.source_leaf_id,
                task: lane_task,
                agent: lane_agent,
                status: if succeeded {
                    SubagentLaneStatus::Completed
                } else {
                    SubagentLaneStatus::Failed
                },
                messages: result
                    .as_ref()
                    .map(|result| result.messages.clone())
                    .unwrap_or_default(),
                error: result
                    .as_ref()
                    .ok()
                    .and_then(|result| result.error.clone())
                    .or_else(|| result.as_ref().err().cloned()),
            };
            Ok((result, lane))
        }
    };
    let results = if parallel {
        futures::future::join_all(
            tasks
                .iter()
                .cloned()
                .enumerate()
                .map(|(task_index, task)| run_one(task_index, task)),
        )
        .await
    } else {
        let mut previous = String::new();
        let mut results = Vec::with_capacity(tasks.len());
        for (task_index, task) in tasks.iter().cloned().enumerate() {
            let task = AgentRunTask {
                agent: task.agent,
                task: task.task.replace("{previous}", &previous),
                instructions: task.instructions,
                tools: task.tools,
                model: task.model,
            };
            let result = run_one(task_index, task).await?;
            if let Ok(output) = &result.0 {
                previous = output.output.clone();
            }
            results.push(Ok(result));
        }
        results
    };
    let results = results.into_iter().collect::<Result<Vec<_>, String>>()?;
    let (tool_results, lanes): (Vec<_>, Vec<_>) = results.into_iter().unzip();
    let thinking = tool_results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .flat_map(|result| result.thinking.clone())
        .collect();
    Ok((
        format_subagent_results(tasks, tool_results, &lanes),
        thinking,
        lanes,
    ))
}

async fn checkpoint_new_subagent_messages(
    session_file: Option<&Path>,
    lane_name: &str,
    run_id: &str,
    state: &Arc<tokio::sync::Mutex<AgentState>>,
    checkpoint_cursor: &mut usize,
) -> Result<(), String> {
    let messages = state.lock().await.messages.clone();
    if let Some(path) = session_file {
        let mut journal = HarnessJournal::open(path)?;
        journal.checkpoint(lane_name, run_id, &messages[*checkpoint_cursor..])?;
    }
    *checkpoint_cursor = messages.len();
    Ok(())
}

async fn consume_subagent_turn_checkpoints(
    mut events: broadcast::Receiver<AgentEvent>,
    session_file: Option<PathBuf>,
    lane_name: String,
    run_id: String,
    state: Arc<tokio::sync::Mutex<AgentState>>,
    initial_checkpoint_cursor: usize,
) -> Result<usize, String> {
    let mut checkpoint_cursor = initial_checkpoint_cursor;
    while let Ok(event) = events.recv().await {
        if matches!(&event, AgentEvent::TurnEnd { .. }) {
            checkpoint_new_subagent_messages(
                session_file.as_deref(),
                &lane_name,
                &run_id,
                &state,
                &mut checkpoint_cursor,
            )
            .await?;
        }
        if matches!(&event, AgentEvent::AgentEnd { .. }) {
            break;
        }
    }
    Ok(checkpoint_cursor)
}

async fn checkpoint_subagent_final_snapshot(
    session_file: Option<&Path>,
    lane_name: &str,
    run_id: &str,
    state: &Arc<tokio::sync::Mutex<AgentState>>,
    checkpoint_cursor: &mut usize,
) -> Result<(), String> {
    checkpoint_new_subagent_messages(session_file, lane_name, run_id, state, checkpoint_cursor)
        .await
}

fn accept_completed_subagent_lanes(
    completed_lanes: &Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
    lanes: Vec<CompletedSubagentLane>,
) -> Result<(), String> {
    completed_lanes
        .lock()
        .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?
        .extend(lanes);
    Ok(())
}

async fn run_subagent_task(
    config: AgentConfig,
    task: String,
    context: SubagentRunContext,
    run_id: u64,
    task_index: usize,
    identity: SubagentLaneIdentity,
    resume_messages: Vec<AgentMessage>,
) -> Result<SubagentResult, String> {
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| context.parent_model.clone());
    let lane_name = identity.lane_name.clone();
    let journal_run_id = identity.run_id.clone();
    let mut agent = Agent::new(
        context.api_key.clone(),
        context.account_id.clone(),
        model.clone(),
    );
    if let Some(tools) = config.tools.clone() {
        agent
            .loop_engine
            .set_allowed_tool_names(Some(tools.into_iter().collect()));
    }
    let system_prompt = format!(
        "{}\n\nYou are an isolated subagent working in {}. Complete only the assigned task and return a concise final report to your parent agent.",
        config.system_prompt,
        context.work_dir.display(),
    );
    agent.set_system_prompt(system_prompt).await;
    let is_recovery = !resume_messages.is_empty();
    if is_recovery {
        agent.loop_engine.state.lock().await.messages.extend(
            resume_messages
                .iter()
                .filter(|message| !matches!(message, AgentMessage::System { .. }))
                .cloned(),
        );
    }
    agent.loop_engine.work_dir = Some(context.work_dir.clone());
    if let Some(session_file) = context.session_file.clone() {
        let assistant_file = session_file.clone();
        let assistant_lane = lane_name.clone();
        let assistant_run = journal_run_id.clone();
        agent.loop_engine.assistant_message_recorder = Some(Arc::new(move |message| {
            let session_file = assistant_file.clone();
            let lane = assistant_lane.clone();
            let run_id = assistant_run.clone();
            Box::pin(async move {
                let mut journal = HarnessJournal::open(&session_file)?;
                journal
                    .append_message_to_lane(&lane, &run_id, message)
                    .map(|_| ())
            })
        }));
        let intent_file = session_file.clone();
        let intent_lane = lane_name.clone();
        let intent_run = journal_run_id.clone();
        agent.loop_engine.tool_intent_recorder =
            Some(Arc::new(move |tool_call_id, tool_name, arguments| {
                let session_file = intent_file.clone();
                let lane_name = intent_lane.clone();
                let journal_run_id = intent_run.clone();
                let tool_call_id = tool_call_id.to_string();
                let tool_name = tool_name.to_string();
                let arguments = arguments.to_string();
                Box::pin(async move {
                    let effective_args = serde_json::from_str(&arguments).map_err(|error| {
                        format!("Failed to parse child tool intent arguments: {error}")
                    })?;
                    let mut journal = HarnessJournal::open(&session_file)?;
                    journal.tool_started_on_lane(
                        &lane_name,
                        &journal_run_id,
                        &tool_call_id,
                        &tool_name,
                        effective_args,
                    )
                })
            }));
        let tool_message_file = session_file.clone();
        let tool_message_lane = lane_name.clone();
        let tool_message_run = journal_run_id.clone();
        agent.loop_engine.tool_message_recorder = Some(Arc::new(move |message| {
            let session_file = tool_message_file.clone();
            let lane = tool_message_lane.clone();
            let run_id = tool_message_run.clone();
            Box::pin(async move {
                let mut journal = HarnessJournal::open(&session_file)?;
                journal
                    .append_message_to_lane(&lane, &run_id, message)
                    .map(|_| ())
            })
        }));
        let usage_file = session_file.clone();
        let usage_run = journal_run_id.clone();
        agent.loop_engine.provider_usage_recorder = Some(Arc::new(move |usage| {
            let session_file = usage_file.clone();
            let run_id = usage_run.clone();
            Box::pin(async move {
                let mut journal = HarnessJournal::open(&session_file)?;
                journal.record_provider_usage(&run_id, usage)
            })
        }));
        let discarded_file = session_file.clone();
        let discarded_run = journal_run_id.clone();
        agent.loop_engine.provider_discarded_usage_recorder = Some(Arc::new(move |usage| {
            let session_file = discarded_file.clone();
            let run_id = discarded_run.clone();
            Box::pin(async move {
                let mut journal = HarnessJournal::open(&session_file)?;
                journal.record_discarded_usage(&run_id, usage)
            })
        }));
    }

    let policy = Arc::new(tokio::sync::Mutex::new(
        if config.tools.as_ref().is_some_and(|tools| {
            !tools
                .iter()
                .any(|tool| matches!(tool.as_str(), "write_file" | "edit_file" | "write" | "edit"))
        }) {
            ToolPolicy::ReadOnly
        } else {
            ToolPolicy::FullAccess
        },
    ));
    let agent_work = AgentWorkScheduler::default();
    let broker_dispatcher = build_broker_dispatcher(
        policy.clone(),
        context.extensions.clone(),
        false,
        context.work_dir.clone(),
        agent.loop_engine.event_tx.clone(),
        agent_work.clone(),
        None,
    )
    .0;
    agent.loop_engine.before_tool_call_hook = Some(Arc::new(ExtensionBeforeToolHook {
        tool_policy: policy,
        extensions: context.extensions.clone(),
        broker_dispatcher: broker_dispatcher.clone(),
    }));
    agent.loop_engine.after_tool_call_hook = Some(Arc::new(ExtensionAfterToolHook {
        extensions: context.extensions.clone(),
        broker_dispatcher,
    }));

    #[cfg(test)]
    if let Some(observer) = context.scheduler_observer.as_ref() {
        if is_recovery
            && resume_messages
                .iter()
                .any(|message| matches!(message, AgentMessage::Tool { .. }))
        {
            return Ok(SubagentResult {
                output: "test subagent result".into(),
                thinking: Vec::new(),
                inner_tools: Vec::new(),
                error: None,
                messages: resume_messages,
            });
        }
        if let Some(tool_observer) = context.child_tool_observer.as_ref() {
            agent.loop_engine.register_tool_executor(Arc::new(
                DeterministicSubagentToolExecutor {
                    observed: tool_observer.clone(),
                },
            ))?;
            let tool_results = agent
                .loop_engine
                .execute_tools(&[threadlane_provider::openai::ToolCall {
                    id: "test-child-tool".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "test_child_tool".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                }])
                .await;
            if tool_results[0].is_error {
                return Err(tool_results[0].content.clone());
            }
        }
        let scheduler = AgentWorkScheduler::default();
        scheduler.set_test_observer(observer.clone());
        scheduler.schedule(if is_recovery {
            AgentWork::RequestTurn(SUBAGENT_RECOVERY_PROMPT.into())
        } else {
            AgentWork::QueueMessage {
                content: "test subagent follow-up".into(),
                images: Vec::new(),
            }
        });
        let observed_model = model.clone();
        let _ = scheduler.run(&mut agent).await;
        let mut messages = if is_recovery {
            resume_messages.clone()
        } else {
            vec![AgentMessage::User {
                content: task.clone(),
            }]
        };
        messages.push(AgentMessage::Assistant {
            content: Some("test subagent result".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });
        return Ok(SubagentResult {
            output: format!("test subagent result ({observed_model})"),
            thinking: Vec::new(),
            inner_tools: Vec::new(),
            error: None,
            messages,
        });
    }

    // The GUI subscribes only to the parent agent. Relay child lifecycle,
    // reasoning, and tool events so users can see subagent progress live.
    // Assistant text stays local and is returned below as one labelled result.
    let mut ui_events = agent.subscribe();
    let ui_event_prefix = format!("subagent-{run_id}:{task_index}:",);
    let event_tx_clone = context.parent_event_tx.clone();
    tokio::spawn(async move {
        while let Ok(event) = ui_events.recv().await {
            if let Some(event) = subagent_ui_event(event, &ui_event_prefix) {
                let _ = event_tx_clone.send(event);
            }
        }
    });

    // Persist only completed child turns; partial stream deltas stay in memory.
    let checkpoint_events = agent.subscribe();
    let checkpoint_state = agent.loop_engine.state.clone();
    let checkpoint_session_file = context.session_file.clone();
    let checkpoint_lane_name = lane_name.clone();
    let checkpoint_run_id = journal_run_id.clone();
    let initial_checkpoint_cursor = agent.loop_engine.state.lock().await.messages.len();
    let checkpoint_task = tokio::spawn(consume_subagent_turn_checkpoints(
        checkpoint_events,
        checkpoint_session_file,
        checkpoint_lane_name,
        checkpoint_run_id,
        checkpoint_state,
        initial_checkpoint_cursor,
    ));

    // Preserve provider and tool-loop errors in the command result as well.
    let mut events = agent.subscribe();
    agent
        .prompt(if is_recovery {
            SUBAGENT_RECOVERY_PROMPT
        } else {
            &task
        })
        .await;
    while agent_work.run(&mut agent).await {}

    let mut checkpoint_cursor = checkpoint_task
        .await
        .map_err(|error| format!("Child turn checkpoint task failed: {error}"))??;
    checkpoint_subagent_final_snapshot(
        context.session_file.as_deref(),
        &lane_name,
        &journal_run_id,
        &agent.loop_engine.state,
        &mut checkpoint_cursor,
    )
    .await?;

    let mut error = None;
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::AgentError { error: message } = event {
            error = Some(message);
        }
    }
    if error.is_some() {
        if config.model.is_some() && model != context.parent_model {
            let mut fallback_config = config.clone();
            fallback_config.model = None;
            return Box::pin(run_subagent_task(
                fallback_config,
                task,
                context,
                run_id,
                task_index,
                identity,
                resume_messages,
            ))
            .await;
        }
    }

    let state = agent.get_state().await;
    let output = state
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            AgentMessage::Assistant {
                content: Some(content),
                ..
            } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let thinking: Vec<AgentMessage> = state
        .messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::Custom { custom_type, .. } if custom_type == "thinking"))
        .cloned()
        .collect();
    let mut inner_tools = Vec::new();
    for message in &state.messages {
        match message {
            AgentMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                for call in calls {
                    inner_tools.push(SubagentInnerTool {
                        id: call.id.clone(),
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                        output: String::new(),
                        is_error: false,
                    });
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                content,
                is_error,
                ..
            } => {
                if let Some(tool) = inner_tools.iter_mut().find(|t| &t.id == tool_call_id) {
                    tool.output = content.clone();
                    tool.is_error = *is_error;
                }
            }
            _ => {}
        }
    }
    let completion_error = error
        .map(|error| format!("Subagent '{}' failed: {error}", config.name))
        .or_else(|| {
            output.is_empty().then(|| {
                format!(
                    "Subagent '{}' completed without a final text response.",
                    config.name
                )
            })
        });
    Ok(SubagentResult {
        output: completion_error.clone().unwrap_or(output),
        thinking,
        inner_tools,
        error: completion_error,
        messages: state
            .messages
            .into_iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .collect(),
    })
}

fn subagent_ui_event(event: AgentEvent, tool_call_prefix: &str) -> Option<AgentEvent> {
    match event {
        // Parent lifecycle and the outer subagent tool own GUI status. Relaying a
        // child's lifecycle would mark a parallel delegation ready or failed
        // while sibling tasks and the parent turn are still running.
        AgentEvent::AgentStart
        | AgentEvent::AgentEnd { .. }
        | AgentEvent::AgentError { .. }
        | AgentEvent::TurnStart { .. }
        | AgentEvent::TurnEnd { .. }
        | AgentEvent::SubagentQueued { .. }
        | AgentEvent::SubagentStarted { .. }
        | AgentEvent::SubagentFinished { .. }
        | AgentEvent::SubagentRecovery { .. } => None,
        // Keep child prose and reasoning inside the child session. The final
        // labelled result renders it under the matching task after completion.
        AgentEvent::MessageUpdate { .. } => None,
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            name,
            arguments,
        } => Some(AgentEvent::ToolExecutionStart {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            name,
            arguments,
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
        } => Some(AgentEvent::ToolExecutionUpdate {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            partial_result,
        }),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            name,
            result,
        } => Some(AgentEvent::ToolExecutionEnd {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            name,
            result,
        }),
        event => Some(event),
    }
}

#[derive(Clone, Debug)]
struct SubagentInnerTool {
    id: String,
    name: String,
    arguments: String,
    output: String,
    is_error: bool,
}

#[derive(Clone, Debug)]
struct SubagentResult {
    output: String,
    thinking: Vec<AgentMessage>,
    inner_tools: Vec<SubagentInnerTool>,
    error: Option<String>,
    messages: Vec<AgentMessage>,
}

fn tool_target_preview(name: &str, arguments: &str) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(arguments).ok();
    let get_str = |key: &str| {
        parsed
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_str)
    };
    let target = match name {
        "read_file" | "write_file" | "edit_file" | "edit_file_hashline" => get_str("path")
            .or_else(|| get_str("file_path"))
            .unwrap_or(arguments),
        "list_dir" => get_str("path").unwrap_or(arguments),
        "run_command" => get_str("command").unwrap_or(arguments),
        _ => arguments,
    };
    if target.chars().count() > 60 {
        target.chars().take(60).collect::<String>()
    } else {
        target.to_string()
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SubagentSessionData {
    run_id: String,
    task: String,
    agent: String,
    status: String,
    thinking: String,
    inner_tools: Vec<SubagentInnerToolData>,
    output: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SubagentInnerToolData {
    name: String,
    target_preview: String,
    is_error: bool,
}

fn format_subagent_results(
    tasks: Vec<AgentRunTask>,
    results: Vec<Result<SubagentResult, String>>,
    lanes: &[CompletedSubagentLane],
) -> String {
    let sessions: Vec<SubagentSessionData> = tasks
        .into_iter()
        .zip(results)
        .zip(lanes)
        .map(|((task, result), lane)| match result {
            Ok(res) => {
                let mut thinking = String::new();
                for think_msg in &res.thinking {
                    if let AgentMessage::Custom { payload, .. } = think_msg {
                        if let Some(text) = payload.get("text").and_then(serde_json::Value::as_str)
                        {
                            thinking.push_str(text);
                            thinking.push_str("\n\n");
                        }
                    }
                }

                let inner_tools = res
                    .inner_tools
                    .into_iter()
                    .map(|tool| SubagentInnerToolData {
                        name: tool.name.clone(),
                        target_preview: tool_target_preview(&tool.name, &tool.arguments),
                        is_error: tool.is_error,
                    })
                    .collect();

                SubagentSessionData {
                    run_id: lane.run_id.clone(),
                    task: task.task,
                    agent: task.agent,
                    status: if res.error.is_some() {
                        "Failed".to_string()
                    } else {
                        "Done".to_string()
                    },
                    thinking: thinking.trim().to_string(),
                    inner_tools,
                    output: res.output,
                }
            }
            Err(error) => SubagentSessionData {
                run_id: lane.run_id.clone(),
                task: task.task,
                agent: task.agent,
                status: "Failed".to_string(),
                thinking: String::new(),
                inner_tools: Vec::new(),
                output: error,
            },
        })
        .collect();

    serde_json::to_string(&sessions)
        .unwrap_or_else(|e| format!("Failed to serialize subagent results: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_broker::CapabilityHandler;
    use crate::wasi_extension::{WasiExtension, WasiExtensionInvocation, WasiExtensionResponse};
    use std::sync::Mutex;
    use std::time::{Duration as StdDuration, Instant};

    #[test]
    fn lagged_generation_event_drain_is_recoverable() {
        assert_eq!(
            generation_event_drain_error(broadcast::error::TryRecvError::Lagged(3)),
            None
        );
        assert_eq!(
            generation_event_drain_error(broadcast::error::TryRecvError::Empty),
            Some("generation ended without a durable AgentEnd event")
        );
    }

    #[test]
    fn harness_journal_round_trips_foreground_operation_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-1", None).unwrap();
        journal.finish("run-1", OpOutcome::Completed, None).unwrap();
        let reopened = HarnessJournal::open(&path).unwrap();
        assert_eq!(reopened.store.records().len(), 2);
        assert!(reopened
            .store
            .records()
            .windows(2)
            .all(|pair| pair[0].seq() < pair[1].seq()));
    }

    #[test]
    fn v2_only_subagent_records_are_recoverable_without_the_legacy_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::File::create(&path).unwrap();
        let mut harness = AgentHarness::new(JsonlStore::open(&path).unwrap());
        harness
            .start_operation_on_lane("subagent-lane", "child-run", None, OperationIntent::Run)
            .unwrap();
        harness.drive_one().unwrap();

        let records = recover_v2_subagent_records(&path).unwrap();
        assert!(records.iter().any(|record| {
            matches!(record, OpRecord::OperationStarted { id, lane, .. } if id == "child-run" && lane == "subagent-lane")
        }));
        assert!(JsonlStore::open(&path)
            .unwrap()
            .records()
            .iter()
            .any(|record| record.id() == "child-run"));
    }

    #[test]
    fn v2_recovery_ignores_foreground_operations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .start_with_prompt("foreground", AgentMessage::user("hello", vec![]))
            .unwrap();

        assert!(recover_v2_subagent_records(&path).unwrap().is_empty());
    }

    #[test]
    fn idle_saved_sessions_start_with_recovery_complete() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::File::create(&session_file).unwrap();
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);

        let agent = CodingAgent::new(options);

        assert!(matches!(
            agent.interrupted_subagent_recovery,
            InterruptedSubagentRecoveryState::Complete
        ));
    }

    #[test]
    fn harness_journal_reuses_the_provisioned_assistant_result_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .start_with_prompt("run-1", AgentMessage::user("hello", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: Some("world".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            })
            .unwrap();
        let attempts: Vec<_> = journal
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::StepAttempt {
                    run_id,
                    result_entry_id,
                    ..
                } if run_id == "run-1" => Some(result_entry_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(attempts, vec!["entry-run-1-assistant-1"]);
        assert!(journal.store.entries().iter().any(|entry| {
            entry.id == "entry-run-1-assistant-1"
                && matches!(entry.message, AgentMessage::Assistant { .. })
        }));
    }

    #[test]
    fn harness_journal_attaches_the_next_turn_to_its_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();

        journal
            .start_with_prompt("run-1", AgentMessage::user("first", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: Some("one".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            })
            .unwrap();
        journal.finish("run-1", OpOutcome::Completed, None).unwrap();

        journal
            .start_with_prompt("run-2", AgentMessage::user("second", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: Some("two".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            })
            .unwrap();

        let prompt_id = "entry-run-2-user";
        assert_eq!(
            journal
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == "entry-run-2-assistant-1")
                .and_then(|entry| entry.parent_id.as_deref()),
            Some(prompt_id)
        );
    }

    #[test]
    fn harness_journal_commits_assistant_intent_before_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .start_with_prompt("run-1", AgentMessage::user("hello", vec![]))
            .unwrap();

        let result_id = journal.prepare_assistant_attempt("run-1").unwrap();
        assert_eq!(result_id, "entry-run-1-assistant-1");
        assert!(journal
            .store
            .entries()
            .iter()
            .all(|entry| { !matches!(entry.message, AgentMessage::Assistant { .. }) }));
        assert!(journal.store.records().iter().any(|record| {
            matches!(
                record,
                HarnessRecord::StepAttempt {
                    run_id,
                    attempt: 1,
                    result_entry_id,
                    ..
                } if run_id == "run-1" && result_entry_id == &result_id
            )
        }));

        journal
            .append_message(AgentMessage::Assistant {
                content: Some("world".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            })
            .unwrap();
        assert!(journal
            .store
            .entries()
            .iter()
            .any(|entry| entry.id == result_id));
    }

    #[test]
    fn harness_journal_closes_a_tool_at_result_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .start_with_prompt("run-1", AgentMessage::user("hello", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![provider_tool_call(
                    "call-1",
                    "read_file",
                    serde_json::json!({"path": "README.md"}),
                )]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        journal
            .append_tool_intent(
                "run-1",
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .unwrap();
        let result = AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "contents".into(),
            is_error: false,
            terminate: false,
        };
        journal.append_message(result.clone()).unwrap();
        journal.finish_tool_message("run-1", &result).unwrap();

        assert!(journal.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::ToolFinished {
                run_id,
                tool_call_id,
                ..
            } if run_id == "run-1" && tool_call_id == "call-1"
        )));
    }

    #[test]
    fn duplicate_tool_intent_does_not_rerun_before_tool_hook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        let hook_calls = Arc::new(AtomicU64::new(0));
        let hook_calls_for_handler = hook_calls.clone();
        journal
            .store
            .hooks_mut()
            .register(
                HookKind::BeforeTool,
                "count-before-tool",
                Arc::new(move |_| {
                    hook_calls_for_handler.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
            )
            .unwrap();
        journal
            .start_with_prompt("run-1", AgentMessage::user("hello", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![provider_tool_call(
                    "call-1",
                    "read_file",
                    serde_json::json!({"path": "README.md"}),
                )]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        journal
            .append_tool_intent(
                "run-1",
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .unwrap();
        journal
            .append_tool_intent(
                "run-1",
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .unwrap();

        assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn foreground_resume_replays_safe_tool_through_harness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal
            .start_with_prompt("run-1", AgentMessage::user("inspect", vec![]))
            .unwrap();
        journal
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![provider_tool_call(
                    "call-1",
                    "read_file",
                    serde_json::json!({"path": "session.jsonl"}),
                )]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        journal
            .append_tool_intent(
                "run-1",
                "call-1",
                "read_file",
                serde_json::json!({"path": "session.jsonl"}),
            )
            .unwrap();
        drop(journal);

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(path.clone());
        let mut coding_agent = CodingAgent::new(options);
        assert!(coding_agent
            .recover_harness_tool_batch("run-1")
            .await
            .unwrap());

        let store = JsonlStore::open(&path).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::Usage {
                cause: threadlane_agent::harness::UsageCause::Replay,
                tool_call_id: Some(tool_call_id),
                ..
            } if tool_call_id == "call-1"
        )));
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::ToolFinished {
                run_id,
                tool_call_id,
                ..
            } if run_id == "run-1" && tool_call_id == "call-1"
        )));
        assert!(store.entries().iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Tool { tool_call_id, .. } if tool_call_id == "call-1"
        )));
    }

    #[test]
    fn harness_run_ids_skip_persisted_ids_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("foreground-1", None).unwrap();
        journal
            .finish("foreground-1", OpOutcome::Completed, None)
            .unwrap();

        let mut reopened = HarnessJournal::open(&path).unwrap();
        let next = reopened.unique_run_id("foreground").unwrap();
        assert_ne!(next, "foreground-1");
        reopened.start(&next, None).unwrap();
    }

    #[test]
    fn harness_retry_survives_restart_and_consumes_before_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-1", None).unwrap();
        assert_eq!(journal.schedule_retry("run-1", "timeout").unwrap(), 1);
        drop(journal);

        let mut reopened = HarnessJournal::open(&path).unwrap();
        assert!(Reducer::reduce(&reopened.store)
            .unwrap()
            .lane("main")
            .unwrap()
            .retry
            .is_some());
        assert_eq!(reopened.begin_retry("run-1").unwrap(), 1);
        assert_eq!(
            Reducer::reduce(&reopened.store)
                .unwrap()
                .lane("main")
                .unwrap()
                .attempts,
            1
        );
    }

    #[test]
    fn retryable_generation_error_markers_are_narrow() {
        assert!(is_retryable_generation_error("provider timeout"));
        assert!(is_retryable_generation_error("HTTP status 503"));
        assert!(!is_retryable_generation_error("invalid request"));
    }

    #[test]
    fn harness_journal_records_the_assistant_attempt_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-1", None).unwrap();
        journal
            .store
            .append_entry(threadlane_agent::harness::Entry {
                id: "assistant-1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 3,
                timestamp: 3,
                message: AgentMessage::Assistant {
                    content: Some("done".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
                terminate: false,
            })
            .unwrap();
        journal
            .record_assistant_attempt(
                "run-1",
                TokenUsage {
                    output_tokens: 2,
                    total_tokens: 2,
                    ..TokenUsage::default()
                },
            )
            .unwrap();
        let reopened = HarnessJournal::open(&path).unwrap();
        assert!(reopened.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::StepAttempt { run_id, result_entry_id, .. }
                if run_id == "run-1" && result_entry_id == "assistant-1"
        )));
        assert!(reopened.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::Usage { run_id: Some(run_id), usage, .. }
                if run_id == "run-1" && usage.output_tokens == 2
        )));
    }

    #[test]
    fn harness_journal_records_a_completed_tool_batch_in_source_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-tools", None).unwrap();
        journal
            .store
            .append_entry(threadlane_agent::harness::Entry {
                id: "assistant-tools".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: "{\"path\":\"README.md\"}".into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                terminate: false,
            })
            .unwrap();
        journal
            .store
            .append_entry(threadlane_agent::harness::Entry {
                id: "tool-result-1".into(),
                parent_id: Some("assistant-tools".into()),
                lane: "main".into(),
                seq: 3,
                timestamp: 3,
                message: AgentMessage::Tool {
                    tool_call_id: "call-1".into(),
                    name: "read_file".into(),
                    content: "contents".into(),
                    is_error: false,
                    terminate: true,
                },
                terminate: true,
            })
            .unwrap();
        journal
            .record_completed_tools_with_termination(
                "run-tools",
                &HashMap::from([(String::from("call-1"), true)]),
            )
            .unwrap();
        let reopened = HarnessJournal::open(&path).unwrap();
        let reduced = Reducer::reduce(&reopened.store).unwrap();
        assert!(reduced
            .lane("main")
            .unwrap()
            .tools
            .iter()
            .all(|tool| tool.completed));
        assert!(reduced
            .lane("main")
            .unwrap()
            .tools
            .iter()
            .any(|tool| tool.terminate));
    }

    #[test]
    fn harness_journal_abort_is_durable_and_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&path).unwrap();
        journal.start("run-1", None).unwrap();
        assert_eq!(journal.request_abort().unwrap().as_deref(), Some("run-1"));
        assert!(journal.recover_abort().unwrap());
        let reopened = HarnessJournal::open(&path).unwrap();
        assert!(reopened
            .store
            .records()
            .iter()
            .any(|record| matches!(record, HarnessRecord::AbortRequested { .. })));
        let attempt_seq = reopened.store.records().iter().find_map(|record| {
            matches!(record, HarnessRecord::StepAttempt { run_id, .. } if run_id == "run-1")
                .then_some(record.seq())
        });
        let aborted_entry_seq = reopened.store.entries().iter().find_map(|entry| {
            matches!(
                &entry.message,
                AgentMessage::Assistant {
                    stop_reason: Some(reason),
                    ..
                } if reason == "aborted"
            )
            .then_some(entry.seq)
        });
        assert!(attempt_seq
            .is_some_and(|attempt| { aborted_entry_seq.is_some_and(|entry| attempt < entry) }));
        assert!(reopened.store.records().iter().any(
            |record| matches!(record, HarnessRecord::OperationFinished { run_id, outcome: OperationOutcome::Aborted, .. } if run_id == "run-1")
        ));
    }

    #[tokio::test]
    async fn suspended_harness_with_a_persisted_assistant_finishes_without_replaying_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(path.clone());
        let mut agent = CodingAgent::new(options);
        let mut store = JsonlStore::open(&path).unwrap();
        store
            .append_record(HarnessRecord::OperationStarted {
                id: "run-resume".into(),
                seq: 1,
                lane: "main".into(),
                timestamp: 1,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();
        store
            .append_entry(threadlane_agent::harness::Entry {
                id: "assistant-resume".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Assistant {
                    content: Some("already persisted".into()),
                    tool_calls: None,
                    stop_reason: Some("stop".into()),
                    deferred_handle: None,
                },
                terminate: false,
            })
            .unwrap();

        assert!(agent.resume_suspended_harness().await.unwrap());
        let reopened = JsonlStore::open(&path).unwrap();
        assert!(reopened.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished { run_id, outcome: OperationOutcome::Completed, .. }
                if run_id == "run-resume"
        )));
    }

    #[test]
    fn subagent_journal_allocates_durable_unique_run_ids_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut first = HarnessJournal::open(&session_file).unwrap();
        let first_run = first
            .start_subagent_lane("subagent-1:0", "inspect", Some("node_1"))
            .unwrap();
        first
            .finish_subagent_lane(
                &first_run.lane_name,
                &first_run.run_id,
                OpOutcome::Completed,
                None,
            )
            .unwrap();
        drop(first);

        let mut second = HarnessJournal::open(&session_file).unwrap();
        let second_run = second
            .start_subagent_lane("subagent-1:0", "inspect again", Some("node_1"))
            .unwrap();
        second
            .finish_subagent_lane(
                &second_run.lane_name,
                &second_run.run_id,
                OpOutcome::Completed,
                None,
            )
            .unwrap();

        assert_ne!(first_run.run_id, second_run.run_id);
        assert_ne!(first_run.lane_name, second_run.lane_name);
        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(
            store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::OperationFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn subagent_journal_writes_v2_run_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(session_file.clone());
        let leaf = tree.add_message(AgentMessage::User {
            content: "parent task".into(),
        });
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let run = journal
            .start_subagent_lane("subagent-1:0", "inspect", Some(&leaf))
            .unwrap();
        journal
            .finish_subagent_lane(&run.lane_name, &run.run_id, OpOutcome::Completed, None)
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationStarted { id, lane, .. }
                if id == &run.run_id && lane == &run.lane_name
        )));
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Completed,
                ..
            } if run_id == &run.run_id
        )));
        assert!(store.entries().iter().any(|entry| {
            entry.lane == run.lane_name
                && matches!(&entry.message, AgentMessage::User { content } if content == "inspect")
        }));
        assert!(Reducer::reduce(&store).is_ok());
    }

    #[test]
    fn subagent_journal_reuses_v2_assistant_result_id() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::write(&session_file, "").unwrap();
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let run = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        let assistant = AgentMessage::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            stop_reason: Some("stop".into()),
            deferred_handle: None,
        };

        let entry_id = journal
            .append_message_to_lane(&run.lane_name, &run.run_id, assistant)
            .unwrap();
        let store = JsonlStore::open(&session_file).unwrap();
        let attempt_id = store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::StepAttempt {
                    run_id,
                    result_entry_id,
                    ..
                } if run_id == &run.run_id => Some(result_entry_id.clone()),
                _ => None,
            })
            .unwrap();

        assert_eq!(entry_id, attempt_id);
        assert!(store.entries().iter().any(|entry| entry.id == attempt_id));
        assert!(Reducer::reduce(&store).is_ok());
    }

    #[test]
    fn concurrent_subagent_starts_share_one_sequence_allocator() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        fs::write(&session_file, "").unwrap();

        let runs = std::thread::scope(|scope| {
            (0..8)
                .map(|index| {
                    let file = session_file.clone();
                    scope.spawn(move || {
                        let mut journal = HarnessJournal::open(&file).unwrap();
                        journal
                            .start_subagent_lane(
                                &format!("subagent-1:{index}"),
                                &format!("task {index}"),
                                Some("node_1"),
                            )
                            .unwrap()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert_eq!(
            runs.iter()
                .map(|run| run.run_id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            runs.len()
        );
        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(store.records().len(), 16);
    }

    #[test]
    fn safe_replay_claim_survives_fresh_journal_restore() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .unwrap();
        let records = recover_v2_subagent_records(&session_file).unwrap();
        let pending = threadlane_agent::interrupted_subagent_lanes(&records);
        assert_eq!(pending[0].safe_tools.len(), 1);
        assert_eq!(
            journal
                .claim_safe_replays(&pending[0].safe_tools)
                .unwrap()
                .len(),
            1
        );
        drop(journal);

        let mut restored = HarnessJournal::open(&session_file).unwrap();
        let records = recover_v2_subagent_records(&session_file).unwrap();
        let pending = threadlane_agent::interrupted_subagent_lanes(&records);
        assert!(pending[0].safe_tools.is_empty());
        assert_eq!(pending[0].unsafe_tools.len(), 1);
        assert!(restored
            .claim_safe_replays(&pending[0].safe_tools)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cancellation_rejects_racing_start_and_writes_one_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();

        let _guard = abort_open_subagent_operations(&session_file).unwrap();
        journal
            .finish_subagent_lane(
                &identity.lane_name,
                &identity.run_id,
                OpOutcome::Completed,
                None,
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(
            store
                .records()
                .iter()
                .filter(|record| matches!(
                    record,
                    HarnessRecord::OperationFinished { run_id, .. }
                        if run_id == &identity.run_id
                ))
                .count(),
            1
        );
        assert!(matches!(
            store.records().last(),
            Some(HarnessRecord::OperationFinished {
                outcome: OperationOutcome::Aborted,
                ..
            })
        ));
    }

    #[test]
    fn subagent_journal_persists_start_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();

        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect the repository", None)
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(matches!(
            store.records().first(),
            Some(HarnessRecord::OperationStarted { id, .. })
                if id == &identity.run_id
        ));
        assert!(matches!(
            store.records().get(1),
            Some(HarnessRecord::StepAttempt { .. })
        ));
    }

    #[test]
    fn subagent_journal_tool_started_uses_explicit_empty_anchor_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();

        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::ToolStarted {
                assistant_entry_id,
                ..
            } if !assistant_entry_id.is_empty()
        )));
    }

    #[test]
    fn subagent_journal_checkpoint_skips_system_messages() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let messages = [
            AgentMessage::System {
                content: "system".into(),
            },
            AgentMessage::User {
                content: "task".into(),
            },
            AgentMessage::Assistant {
                content: Some("done".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            },
        ];

        let identity = journal
            .start_subagent_lane("subagent-1:0", "task", None)
            .unwrap();
        journal
            .checkpoint(&identity.lane_name, &identity.run_id, &messages)
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert_eq!(store.entries().len(), 3);
    }

    #[test]
    fn subagent_journal_finish_closes_started_run() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();

        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .finish_subagent_lane(
                &identity.lane_name,
                &identity.run_id,
                OpOutcome::Completed,
                None,
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(matches!(
            store.records().last(),
            Some(HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Completed,
                ..
            }) if run_id == &identity.run_id
        ));
    }

    #[test]
    fn subagent_journal_finish_does_not_duplicate_across_loaded_journals() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut first = HarnessJournal::open(&session_file).unwrap();
        let identity = first
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        let mut second = HarnessJournal::open(&session_file).unwrap();

        first
            .finish_subagent_lane(
                &identity.lane_name,
                &identity.run_id,
                OpOutcome::Completed,
                None,
            )
            .unwrap();
        second
            .finish_subagent_lane(
                &identity.lane_name,
                &identity.run_id,
                OpOutcome::Aborted,
                None,
            )
            .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        let terminals: Vec<_> = store
            .records()
            .iter()
            .filter(|record| {
                matches!(
                    record,
                    HarnessRecord::OperationFinished { run_id, .. }
                        if run_id == &identity.run_id
                )
            })
            .collect();
        assert_eq!(terminals.len(), 1);
        assert!(matches!(
            terminals.first(),
            Some(HarnessRecord::OperationFinished {
                outcome: OperationOutcome::Completed,
                ..
            })
        ));
    }

    #[test]
    fn interrupted_subagent_recovery_does_not_mutate_parent_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "write_file",
                serde_json::json!({}),
            )
            .unwrap();

        let mut tree = SessionTree::new("session");
        tree.add_message(AgentMessage::User {
            content: "parent".into(),
        });
        assert_eq!(tree.get_active_branch_messages().len(), 1);
    }

    #[tokio::test]
    async fn safe_subagent_recovery_replays_tool_once() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let recovered_file = dir.path().join("recovered.txt");
        std::fs::write(&recovered_file, "replayed content").unwrap();
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .checkpoint(
                &identity.lane_name,
                &identity.run_id,
                &[AgentMessage::User {
                    content: "deferred".into(),
                }],
            )
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": recovered_file}),
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let executor_count = Arc::new(AtomicU64::new(0));
        coding_agent.agent.loop_engine.before_tool_call_hook =
            Some(Arc::new(CountingBeforeToolHook {
                count: executor_count.clone(),
            }));
        coding_agent.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));
        let parent = coding_agent.session_tree.add_message(AgentMessage::User {
            content: "parent".into(),
        });

        assert_eq!(
            coding_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            coding_agent.session_tree.active_node_id(),
            Some(parent.as_str())
        );
        assert_eq!(executor_count.load(Ordering::SeqCst), 0);
        assert!(coding_agent.session_tree.nodes.values().any(|node| {
            matches!(
                &node.message,
                AgentMessage::Custom { custom_type, payload }
                    if custom_type == "subagent_lane"
                        && payload.get("run_id").and_then(Value::as_str)
                            == Some(identity.run_id.as_str())
            )
        }));

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Completed,
                ..
            } if run_id == &identity.run_id
        )));
    }

    #[tokio::test]
    async fn interrupted_subagent_recovery_resumes_child_from_latest_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "finish the audit", None)
            .unwrap();
        journal
            .checkpoint(
                &identity.lane_name,
                &identity.run_id,
                &[
                    AgentMessage::User {
                        content: "finish the audit".into(),
                    },
                    AgentMessage::Assistant {
                        content: Some("I inspected the first half.".into()),
                        tool_calls: None,
                        stop_reason: None,
                        deferred_handle: None,
                    },
                ],
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);
        let mut coding_agent = CodingAgent::new(options);
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.set_subagent_work_observer(observed.clone());
        let mut events = coding_agent.agent.subscribe();

        assert_eq!(
            coding_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::RequestTurn(
                "Continue from the recovered checkpoint and finish the assigned task.".into()
            )]
        );
        assert!(coding_agent.session_tree.nodes.values().any(|node| {
            matches!(
                &node.message,
                AgentMessage::Assistant {
                    content: Some(content),
                    ..
                } if content == "test subagent result"
            )
        }));
        let recovery_statuses = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AgentEvent::SubagentRecovery { status, .. } => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            recovery_statuses,
            vec![
                threadlane_agent::SubagentRecoveryStatus::Started,
                threadlane_agent::SubagentRecoveryStatus::Retrying,
                threadlane_agent::SubagentRecoveryStatus::Recovered,
            ]
        );
    }

    #[tokio::test]
    async fn recovered_subagent_branch_uses_persisted_parent_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut original = CodingAgent::new(options);
        let parent = original.session_tree.add_message(AgentMessage::User {
            content: "originating parent".into(),
        });
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "finish the audit", Some(&parent))
            .unwrap();
        journal
            .checkpoint(
                &identity.lane_name,
                &identity.run_id,
                &[AgentMessage::User {
                    content: "finish the audit".into(),
                }],
            )
            .unwrap();
        drop(original);
        drop(journal);

        let mut restarted_options = coding_agent_options(dir.path().to_path_buf());
        restarted_options.session_file = Some(session_file);
        let mut restarted = CodingAgent::new(restarted_options);
        restarted.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));
        assert_eq!(
            restarted
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            restarted.session_tree.active_node_id(),
            Some(parent.as_str())
        );
        let marker = restarted
            .session_tree
            .nodes
            .values()
            .find(|node| {
                matches!(
                    &node.message,
                    AgentMessage::Custom { custom_type, payload }
                        if custom_type == "subagent_lane"
                            && payload.get("run_id").and_then(Value::as_str)
                                == Some(identity.run_id.as_str())
                )
            })
            .unwrap();
        assert_eq!(marker.parent_id.as_deref(), Some(parent.as_str()));
    }

    #[tokio::test]
    async fn materialized_open_subagent_recovery_resumes_without_replaying_safe_tool() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let recovered_file = dir.path().join("recovered.txt");
        std::fs::write(&recovered_file, "replayed content").unwrap();
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": recovered_file}),
            )
            .unwrap();
        let records = recover_v2_subagent_records(&session_file).unwrap();
        let safe_tool = threadlane_agent::interrupted_subagent_lanes(&records)
            .remove(0)
            .safe_tools
            .remove(0);
        let executor_count = Arc::new(AtomicU64::new(0));

        let mut first_options = coding_agent_options(dir.path().to_path_buf());
        first_options.session_file = Some(session_file.clone());
        let mut first_agent = CodingAgent::new(first_options);
        first_agent.agent.loop_engine.before_tool_call_hook =
            Some(Arc::new(CountingBeforeToolHook {
                count: executor_count.clone(),
            }));
        let safe_message = first_agent
            .replay_safe_tools(&[safe_tool])
            .await
            .into_iter()
            .map(|result| {
                let terminate = result.terminates();
                AgentMessage::Tool {
                    tool_call_id: result.tool_call_id,
                    name: result.name,
                    content: result.content,
                    is_error: result.is_error,
                    terminate,
                }
            })
            .next()
            .unwrap();
        assert_eq!(executor_count.load(Ordering::SeqCst), 0);
        journal
            .checkpoint(
                &identity.lane_name,
                &identity.run_id,
                &[safe_message.clone()],
            )
            .unwrap();
        journal.refresh().unwrap();
        journal
            .store
            .finish_existing_tool(
                &identity.run_id,
                HarnessToolResult {
                    call_id: "call-1".into(),
                    name: "read_file".into(),
                    content: "1:6de|replayed content".into(),
                    is_error: false,
                    terminate: false,
                },
            )
            .unwrap();
        journal.store.drive_to_completion().unwrap();
        first_agent
            .session_tree
            .append_passive_branch_in_memory(
                None,
                vec![
                    AgentMessage::Custom {
                        custom_type: "subagent_lane".into(),
                        payload: serde_json::json!({
                            "lane": identity.lane_name.clone(),
                            "run_id": identity.run_id.clone(),
                            "agent": "recovered",
                            "task": "inspect",
                            "status": "completed",
                            "error": null,
                        }),
                    },
                    AgentMessage::User {
                        content: "deferred".into(),
                    },
                    safe_message,
                ],
            )
            .unwrap();
        drop(first_agent);
        drop(journal);

        let mut resumed_options = coding_agent_options(dir.path().to_path_buf());
        resumed_options.session_file = Some(session_file.clone());
        let mut resumed_agent = CodingAgent::new(resumed_options);
        resumed_agent.agent.loop_engine.before_tool_call_hook =
            Some(Arc::new(CountingBeforeToolHook {
                count: executor_count.clone(),
            }));
        assert_eq!(
            resumed_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(executor_count.load(Ordering::SeqCst), 0);
        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished { run_id, .. } if run_id == &identity.run_id
        )));
    }

    #[tokio::test]
    async fn journal_load_failure_blocks_normal_input() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("invalid/session.jsonl");
        std::fs::create_dir_all(&session_file).unwrap();
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);
        let mut coding_agent = CodingAgent::new(options);

        let first_error = coding_agent
            .handle_input_with_images("/subagent", Vec::new())
            .await
            .unwrap()
            .unwrap_err();

        assert!(first_error.contains("Harness Error"));
        assert!(coding_agent.session_tree.nodes.is_empty());
    }

    #[tokio::test]
    async fn mixed_subagent_recovery_aborts_unsafe_tool_after_safe_replay() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let run_command_count = dir.path().join("run-command-count");
        let recovered_file = dir.path().join("recovered.txt");
        std::fs::write(&recovered_file, "replayed content").unwrap();
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "safe-call",
                "read_file",
                serde_json::json!({"path": recovered_file}),
            )
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "unsafe-call",
                "run_command",
                serde_json::json!({"command": format!("printf 1 >> {}", run_command_count.display())}),
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let safe_executor_count = Arc::new(AtomicU64::new(0));
        coding_agent.agent.loop_engine.before_tool_call_hook =
            Some(Arc::new(CountingBeforeToolHook {
                count: safe_executor_count.clone(),
            }));
        assert_eq!(
            coding_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert_eq!(safe_executor_count.load(Ordering::SeqCst), 0);
        assert!(!run_command_count.exists());
        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Aborted,
                ..
            } if run_id == &identity.run_id
        )));
    }

    #[tokio::test]
    async fn unsafe_subagent_recovery_aborts_without_execution() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let run_command_count = dir.path().join("run-command-count");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "call-1",
                "run_command",
                serde_json::json!({"command": format!("printf 1 >> {}", run_command_count.display())}),
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let mut events = coding_agent.agent.subscribe();

        assert_eq!(
            coding_agent
                .recover_interrupted_subagent_lanes()
                .await
                .unwrap(),
            1
        );
        assert!(!run_command_count.exists());

        let recovery_statuses = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AgentEvent::SubagentRecovery { status, .. } => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            recovery_statuses,
            vec![
                threadlane_agent::SubagentRecoveryStatus::Started,
                threadlane_agent::SubagentRecoveryStatus::Aborted,
            ]
        );

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationFinished {
                run_id,
                outcome: OperationOutcome::Aborted,
                ..
            } if run_id == &identity.run_id
        )));
    }

    #[tokio::test]
    async fn recovery_failure_after_started_emits_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let identity = journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .unwrap();
        journal
            .tool_started_on_lane(
                &identity.lane_name,
                &identity.run_id,
                "unsafe-call",
                "run_command",
                serde_json::json!({"command": "pwd"}),
            )
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let coding_agent = CodingAgent::new(options.clone());
        let mut events = coding_agent.agent.subscribe();
        // Cause open error by creating directory at invalid path
        options.session_file = Some(dir.path().join("invalid/session.jsonl"));
        std::fs::create_dir_all(options.session_file.as_ref().unwrap()).unwrap();
        let mut failing_agent = CodingAgent::new(options);

        let error = failing_agent
            .recover_interrupted_subagent_lanes()
            .await
            .unwrap_err();
        assert!(error.contains("Harness Error"));
        let recovery_statuses = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                AgentEvent::SubagentRecovery { status, .. } => Some(status),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(recovery_statuses.is_empty());
    }

    #[tokio::test]
    async fn model_switch_repairs_tool_call_interrupted_by_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let user = AgentMessage::User {
            content: "inspect the repository".into(),
        };
        coding_agent.session_tree.add_message(user.clone());
        {
            let mut state = coding_agent.agent.loop_engine.state.lock().await;
            state.messages.push(user);
            state.messages.push(AgentMessage::Custom {
                custom_type: "thinking".into(),
                payload: serde_json::json!({"text": "planning"}),
            });
            state.messages.push(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![provider_tool_call(
                    "call-interrupted",
                    "read_file",
                    serde_json::json!({"path": "src/main.rs"}),
                )]),
                stop_reason: None,
                deferred_handle: None,
            });
        }

        let output = coding_agent.handle_input("/model next-model").await;

        assert_eq!(output.unwrap().unwrap(), "Switched model to: next-model");
        let state = coding_agent.agent.get_state().await;
        assert_eq!(state.model, "next-model");
        assert_eq!(state.messages.len(), 2);
        assert!(matches!(state.messages[1], AgentMessage::User { .. }));
        assert_eq!(
            coding_agent.session_tree.get_active_branch_messages().len(),
            1
        );
        let (_, codex) = coding_agent.agent.loop_engine.build_api_payloads().await;
        assert!(codex["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["type"] != "function_call"));
    }

    #[tokio::test]
    async fn model_switch_preserves_antigravity_provider_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        let output = coding_agent
            .handle_input("/model antigravity/gemini-3.6-flash")
            .await;

        assert_eq!(
            output.unwrap().unwrap(),
            "Switched model to: antigravity/gemini-3.6-flash"
        );
        let (chat, codex) = coding_agent.agent.loop_engine.build_api_payloads().await;
        assert_eq!(chat["model"], "antigravity/gemini-3.6-flash");
        assert_eq!(codex["model"], "antigravity/gemini-3.6-flash");
        assert_eq!(
            coding_agent.session_tree.model.as_deref(),
            Some("antigravity/gemini-3.6-flash")
        );
    }

    #[tokio::test]
    async fn invalid_command_returns_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        let output = coding_agent.handle_input("/subagent").await;

        assert!(output.unwrap().is_err());
    }

    #[tokio::test]
    async fn persisted_session_history_is_loaded_into_provider_context() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let messages = vec![
            AgentMessage::User {
                content: "Choose a scrollbar behavior".into(),
            },
            AgentMessage::Assistant {
                content: Some("A. Always visible\nB. Visible while scrolling\nC. Hidden".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::User {
                content: "B".into(),
            },
        ];
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(session_file.clone());
        for message in &messages {
            tree.add_message(message.clone());
        }

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);
        let coding_agent = CodingAgent::new(options);

        let state = coding_agent.agent.get_state().await;
        assert!(matches!(
            state.messages.first(),
            Some(AgentMessage::System { .. })
        ));
        assert_eq!(
            serde_json::to_value(&state.messages[1..]).unwrap(),
            serde_json::to_value(&messages).unwrap()
        );

        let (chat, codex) = coding_agent.agent.loop_engine.build_api_payloads().await;
        assert_eq!(chat["messages"][2]["role"], "assistant");
        assert_eq!(chat["messages"][3]["content"], "B");
        assert_eq!(codex["input"][1]["role"], "assistant");
        assert_eq!(codex["input"][2]["content"][0]["text"], "B");
    }

    #[tokio::test]
    async fn sync_session_history_loads_recovered_messages_into_provider_context() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        coding_agent.session_tree.add_message(AgentMessage::User {
            content: "Recovered prompt".into(),
        });

        coding_agent.sync_session_history().await;

        let state = coding_agent.agent.get_state().await;
        assert!(matches!(
            state.messages.last(),
            Some(AgentMessage::User { content }) if content == "Recovered prompt"
        ));
    }

    #[tokio::test]
    async fn replay_safe_tools_executes_read_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovered.txt");
        std::fs::write(&path, "replayed content").unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let live_intents = Arc::new(AtomicU64::new(0));
        let observed_live_intents = live_intents.clone();
        coding_agent.set_tool_intent_recorder(Some(Arc::new(move |_, _, _| {
            let observed_live_intents = observed_live_intents.clone();
            Box::pin(async move {
                observed_live_intents.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        })));
        let record = threadlane_agent::OpRecord::ToolStarted {
            id: "tool-1".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            run_id: "run-1".into(),
            assistant_entry_id: String::new(),
            tool_index: 0,
            tool_call_id: "call-1".into(),
            tool_name: "read_file".into(),
            effective_args: serde_json::json!({"path": path}),
            result_entry_id: "result-1".into(),
            replay: threadlane_agent::ToolReplaySafety::Safe,
        };

        let results = coding_agent.replay_safe_tools(&[record]).await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_call_id, "call-1");
        assert!(results[0].content.contains("replayed content"));
        assert!(!results[0].is_error);
        assert_eq!(live_intents.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn persisted_session_model_overrides_constructor_default() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(session_file.clone());
        tree.add_message(AgentMessage::User {
            content: "continue".into(),
        });
        tree.set_model("antigravity/claude-opus-4-6".into())
            .unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.model = "fallback-model".into();
        options.session_file = Some(session_file);
        let coding_agent = CodingAgent::new(options);

        assert_eq!(
            coding_agent.session_tree.model.as_deref(),
            Some("antigravity/claude-opus-4-6")
        );
        assert_eq!(
            coding_agent.agent.get_state().await.model,
            "antigravity/claude-opus-4-6"
        );
    }

    #[tokio::test]
    async fn v2_model_fact_overrides_legacy_metadata_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.model = "legacy-model".into();
        options.session_file = Some(session_file.clone());
        let mut first = CodingAgent::new(options);
        first
            .set_model("antigravity/provider-model".into())
            .await
            .unwrap();
        drop(first);

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.model = "constructor-default".into();
        options.session_file = Some(session_file);
        let restarted = CodingAgent::new(options);
        assert_eq!(
            restarted.session_tree.model.as_deref(),
            Some("antigravity/provider-model")
        );
        assert_eq!(
            restarted.agent.get_state().await.model,
            "antigravity/provider-model"
        );
    }

    #[tokio::test]
    async fn new_session_path_sets_unique_runtime_identity_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("sessions/session-42.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());

        let mut coding_agent = CodingAgent::new(options);

        assert_eq!(coding_agent.session_tree.session_id, "session-42");
        coding_agent.session_tree.add_message(AgentMessage::User {
            content: "persist me".into(),
        });
        assert_eq!(
            serde_json::to_value(
                SessionTree::load_from_file(&session_file)
                    .unwrap()
                    .get_active_branch_messages()
            )
            .unwrap(),
            serde_json::json!([{
                "role": "user",
                "content": "persist me",
            }])
        );
    }

    #[tokio::test]
    async fn v2_reload_uses_harness_leaf_when_metadata_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut tree = SessionTree::new("session");
        tree.file_path = Some(session_file.clone());
        let first = tree.add_message(AgentMessage::User {
            content: "first".into(),
        });
        let second = tree.add_message(AgentMessage::Assistant {
            content: Some("second".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });
        assert_ne!(first, second);

        let contents = fs::read_to_string(&session_file).unwrap();
        let stale = contents
            .lines()
            .map(|line| {
                if line.contains("\"type\":\"session_metadata\"") {
                    serde_json::from_str::<Value>(line)
                        .map(|mut value| {
                            value["active_node_id"] = Value::String(first.clone());
                            serde_json::to_string(&value).unwrap()
                        })
                        .unwrap()
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&session_file, format!("{stale}\n")).unwrap();

        let mut harness = AgentHarness::new(JsonlStore::open(&session_file).unwrap());
        harness
            .start_operation("run-1", Some(first), OperationIntent::Run)
            .unwrap();
        harness.drive_to_completion().unwrap();

        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file);
        let coding_agent = CodingAgent::new(options);
        assert_eq!(
            coding_agent.session_tree.active_node_id(),
            Some(second.as_str())
        );
    }

    #[test]
    fn subagent_ui_events_do_not_override_parent_lifecycle() {
        assert!(subagent_ui_event(AgentEvent::AgentStart, "child:").is_none());
        assert!(subagent_ui_event(
            AgentEvent::AgentEnd {
                usage: Default::default()
            },
            "child:"
        )
        .is_none());
        assert!(subagent_ui_event(
            AgentEvent::AgentError {
                error: "child failed".into()
            },
            "child:"
        )
        .is_none());
        assert!(subagent_ui_event(
            AgentEvent::SubagentQueued {
                run_id: 1,
                task_index: 0,
                agent: "nested".into(),
                task: "nested task".into(),
            },
            "child:"
        )
        .is_none());

        let reasoning = subagent_ui_event(
            AgentEvent::MessageUpdate {
                text_delta: Some("hidden child prose".into()),
                reasoning_delta: Some("visible progress".into()),
                tool_call_name: None,
            },
            "child:",
        );
        assert!(reasoning.is_none());

        let tool = subagent_ui_event(
            AgentEvent::ToolExecutionStart {
                tool_call_id: "tool".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            },
            "child:",
        );
        assert!(matches!(
            tool,
            Some(AgentEvent::ToolExecutionStart { tool_call_id, .. })
                if tool_call_id == "child:tool"
        ));
    }

    fn handler(capability: &'static str, work_dir: PathBuf) -> HostCapabilityHandler {
        handler_with_scheduler(capability, work_dir, AgentWorkScheduler::default())
    }

    fn handler_with_scheduler(
        capability: &'static str,
        work_dir: PathBuf,
        agent_work: AgentWorkScheduler,
    ) -> HostCapabilityHandler {
        let (event_tx, _) = broadcast::channel(4);
        HostCapabilityHandler {
            capability,
            tool_policy: None,
            extensions: Arc::new(WasiExtensionManager::new()),
            work_dir,
            event_tx,
            allowed_hosts: Arc::new(HashSet::new()),
            agent_work,
            agent_runner: None,
            persist_tool_policy: false,
            managed_processes: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    fn push_unsigned_leb(mut value: u32, bytes: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn push_signed_leb(mut value: i64, bytes: &mut Vec<u8>) {
        loop {
            let byte = (value as u8) & 0x7f;
            value >>= 7;
            let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            bytes.push(if done { byte } else { byte | 0x80 });
            if done {
                break;
            }
        }
    }

    fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
        wasm.push(id);
        push_unsigned_leb(payload.len() as u32, wasm);
        wasm.extend_from_slice(payload);
    }

    fn queue_command_wasm() -> Vec<u8> {
        let manifest = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "name": "queue_command_ext",
            "version": "1.0.0",
            "description": "scheduler integration fixture",
            "capabilities": ["agent"],
            "commands": [{"name": "queue", "description": "queue follow-up"}]
        })
        .to_string();
        let request = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "capability": "agent",
            "operation": "queue_message",
            "arguments": {"content": "standalone queued work"}
        })
        .to_string();
        let response = br#"{"message":"queued"}"#;
        let response_offset = 1024usize;
        let request_offset = 4096usize;
        let request_response_offset = 6000usize;
        let mut data = vec![0; request_response_offset + 1024];
        data[..manifest.len()].copy_from_slice(manifest.as_bytes());
        data[response_offset..response_offset + response.len()].copy_from_slice(response);
        data[request_offset..request_offset + request.len()].copy_from_slice(request.as_bytes());

        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(
            &mut wasm,
            1,
            &[
                4, 0x60, 0, 1, 0x7e, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e, 0x60, 4,
                0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f,
            ],
        );
        let host_module = b"threadlane_host";
        let mut imports = vec![1];
        push_unsigned_leb(host_module.len() as u32, &mut imports);
        imports.extend_from_slice(host_module);
        imports.push(7);
        imports.extend_from_slice(b"request");
        imports.extend_from_slice(&[0, 3]);
        push_section(&mut wasm, 2, &imports);
        push_section(&mut wasm, 3, &[3, 0, 1, 2]);
        push_section(&mut wasm, 5, &[1, 0, 2]);

        let mut exports = vec![4];
        for (name, kind, index) in [
            ("extension_info", 0, 1),
            ("alloc", 0, 2),
            ("execute_command", 0, 3),
            ("memory", 2, 0),
        ] {
            push_unsigned_leb(name.len() as u32, &mut exports);
            exports.extend_from_slice(name.as_bytes());
            exports.extend_from_slice(&[kind, index]);
        }
        push_section(&mut wasm, 7, &exports);

        let mut bodies = Vec::new();
        for body in [
            {
                let mut body = vec![0, 0x42];
                push_signed_leb(manifest.len() as i64, &mut body);
                body.push(0x0b);
                body
            },
            vec![0, 0x41, 0],
            {
                let mut body = vec![0, 0x41];
                push_signed_leb(request_offset as i64, &mut body);
                body.push(0x41);
                push_signed_leb(request.len() as i64, &mut body);
                body.push(0x41);
                push_signed_leb(request_response_offset as i64, &mut body);
                body.push(0x41);
                push_signed_leb(1024, &mut body);
                body.extend_from_slice(&[0x10, 0, 0x1a, 0x42]);
                let packed = ((response_offset as u64) << 32) | response.len() as u64;
                push_signed_leb(packed as i64, &mut body);
                body.push(0x0b);
                body
            },
        ] {
            let mut full = body;
            if full.last() != Some(&0x0b) {
                full.push(0x0b);
            }
            push_unsigned_leb(full.len() as u32, &mut bodies);
            bodies.extend_from_slice(&full);
        }
        let mut code = vec![3];
        code.extend_from_slice(&bodies);
        push_section(&mut wasm, 10, &code);
        let mut data_section = vec![1, 0, 0x41, 0, 0x0b];
        push_unsigned_leb(data.len() as u32, &mut data_section);
        data_section.extend_from_slice(&data);
        push_section(&mut wasm, 11, &data_section);
        wasm
    }

    const CONTINUATION_EXTENSION_NAME: &str = "continuation_tool_ext";
    const CONTINUATION_TOOL_NAME: &str = "continuation_tool";
    const CONTINUATION_TOOL_ARGS: &str = r#"{"sentinel":"same args"}"#;

    fn broker_tool_wasm(
        operation: &str,
        continue_after_broker: bool,
        finish_after_event: bool,
    ) -> Vec<u8> {
        let manifest = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "name": CONTINUATION_EXTENSION_NAME,
            "version": "1.0.0",
            "description": "broker continuation fixture",
            "capabilities": ["tools"],
            "tools": [{
                "name": CONTINUATION_TOOL_NAME,
                "description": "exercise broker continuation",
                "parameters": {"type": "object"}
            }]
        })
        .to_string();
        let request = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "capability": "tools",
            "operation": operation,
            "arguments": Value::Null
        })
        .to_string();
        let initial_response = serde_json::json!({
            "message": "waiting for broker response",
            "continue_after_broker": continue_after_broker
        })
        .to_string();
        let final_response = serde_json::json!({
            "message": "post-processed broker response"
        })
        .to_string();
        let initial_invocation_len = serde_json::to_vec(&WasiExtensionInvocation {
            api_version: BROKER_API_VERSION,
            kind: "tool".into(),
            name: CONTINUATION_TOOL_NAME.into(),
            arguments: serde_json::from_str(CONTINUATION_TOOL_ARGS).unwrap(),
            state: serde_json::json!({}),
            events: Vec::new(),
        })
        .unwrap()
        .len();

        let initial_response_offset = 1024usize;
        let final_response_offset = 2048usize;
        let request_offset = 4096usize;
        let request_response_offset = 6144usize;
        let mut data = vec![0; request_response_offset + 1024];
        data[..manifest.len()].copy_from_slice(manifest.as_bytes());
        data[initial_response_offset..initial_response_offset + initial_response.len()]
            .copy_from_slice(initial_response.as_bytes());
        data[final_response_offset..final_response_offset + final_response.len()]
            .copy_from_slice(final_response.as_bytes());
        data[request_offset..request_offset + request.len()].copy_from_slice(request.as_bytes());

        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(
            &mut wasm,
            1,
            &[
                4, 0x60, 0, 1, 0x7e, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e, 0x60, 4,
                0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f,
            ],
        );
        let host_module = b"threadlane_host";
        let mut imports = vec![1];
        push_unsigned_leb(host_module.len() as u32, &mut imports);
        imports.extend_from_slice(host_module);
        imports.push(7);
        imports.extend_from_slice(b"request");
        imports.extend_from_slice(&[0, 3]);
        push_section(&mut wasm, 2, &imports);
        push_section(&mut wasm, 3, &[3, 0, 1, 2]);
        push_section(&mut wasm, 5, &[1, 0, 2]);

        let mut exports = vec![4];
        for (name, kind, index) in [
            ("extension_info", 0, 1),
            ("alloc", 0, 2),
            ("execute_tool", 0, 3),
            ("memory", 2, 0),
        ] {
            push_unsigned_leb(name.len() as u32, &mut exports);
            exports.extend_from_slice(name.as_bytes());
            exports.extend_from_slice(&[kind, index]);
        }
        push_section(&mut wasm, 7, &exports);

        let mut extension_info = vec![0, 0x42];
        push_signed_leb(manifest.len() as i64, &mut extension_info);
        extension_info.push(0x0b);
        let alloc = vec![0, 0x41, 0, 0x0b];
        let mut execute_tool = vec![0];
        if finish_after_event {
            execute_tool.extend_from_slice(&[0x20, 1, 0x41]);
            push_signed_leb(initial_invocation_len as i64, &mut execute_tool);
            execute_tool.extend_from_slice(&[0x4b, 0x04, 0x7e, 0x42]);
            let packed = ((final_response_offset as u64) << 32) | final_response.len() as u64;
            push_signed_leb(packed as i64, &mut execute_tool);
            execute_tool.push(0x05);
        }
        execute_tool.push(0x41);
        push_signed_leb(request_offset as i64, &mut execute_tool);
        execute_tool.push(0x41);
        push_signed_leb(request.len() as i64, &mut execute_tool);
        execute_tool.push(0x41);
        push_signed_leb(request_response_offset as i64, &mut execute_tool);
        execute_tool.push(0x41);
        push_signed_leb(1024, &mut execute_tool);
        execute_tool.extend_from_slice(&[0x10, 0, 0x1a, 0x42]);
        let packed = ((initial_response_offset as u64) << 32) | initial_response.len() as u64;
        push_signed_leb(packed as i64, &mut execute_tool);
        if finish_after_event {
            execute_tool.push(0x0b);
        }
        execute_tool.push(0x0b);

        let mut code = vec![3];
        for body in [extension_info, alloc, execute_tool] {
            push_unsigned_leb(body.len() as u32, &mut code);
            code.extend_from_slice(&body);
        }
        push_section(&mut wasm, 10, &code);
        let mut data_section = vec![1, 0, 0x41, 0, 0x0b];
        push_unsigned_leb(data.len() as u32, &mut data_section);
        data_section.extend_from_slice(&data);
        push_section(&mut wasm, 11, &data_section);
        wasm
    }

    fn coding_agent_options(work_dir: PathBuf) -> CodingAgentOptions {
        CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir,
            session_file: None,
            system_prompt: SystemPromptConfig::default(),
        }
    }

    #[test]
    fn set_credentials_updates_the_running_agent() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        agent.set_credentials("new-token".into(), Some("new-account".into()));

        assert_eq!(agent.agent.loop_engine.api_key, "new-token");
        assert_eq!(
            agent.agent.loop_engine.account_id.as_deref(),
            Some("new-account")
        );
    }

    #[test]
    fn cancel_keeps_subagent_cancellation_active_until_the_next_submission() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut agent = CodingAgent::new(options);
        let mut journal = HarnessJournal::open(&session_file).unwrap();
        let mut events = agent.subscribe();

        agent.cancel().unwrap();

        assert!(journal
            .start_subagent_lane("subagent-1:0", "inspect", None)
            .is_err());
        assert!(matches!(
            events.try_recv(),
            Ok(AgentEvent::AgentError { error }) if error == "Generation cancelled"
        ));
    }

    #[tokio::test]
    async fn cancel_aborts_active_run_without_a_session_file_before_the_next_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let cancellation = agent.cancellation_handle();
        let mut events = agent.subscribe();
        let event_tx = agent.agent.loop_engine.event_tx.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let run = tokio::spawn(async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            let _ = event_tx.send(AgentEvent::MessageUpdate {
                text_delta: Some("late".into()),
                reasoning_delta: None,
                tool_call_name: None,
            });
        });
        started_rx.await.unwrap();
        cancellation.track_active_run(run.abort_handle()).unwrap();

        cancellation.cancel().unwrap();

        assert!(run.await.unwrap_err().is_cancelled());
        assert!(matches!(
            events.try_recv(),
            Ok(AgentEvent::AgentError { error }) if error == "Generation cancelled"
        ));
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        let event_tx = agent.agent.loop_engine.event_tx.clone();
        let next = tokio::spawn(async move {
            let _ = event_tx.send(AgentEvent::MessageUpdate {
                text_delta: Some("next".into()),
                reasoning_delta: None,
                tool_call_name: None,
            });
        });
        cancellation.track_active_run(next.abort_handle()).unwrap();
        next.await.unwrap();
        assert!(matches!(
            events.try_recv(),
            Ok(AgentEvent::MessageUpdate { text_delta: Some(text), .. }) if text == "next"
        ));
    }

    fn provider_tool_call(
        id: &str,
        name: &str,
        arguments: serde_json::Value,
    ) -> threadlane_provider::openai::ToolCall {
        threadlane_provider::openai::ToolCall {
            id: id.into(),
            r#type: "function".into(),
            function: threadlane_provider::openai::ToolCallFunction {
                name: name.into(),
                arguments: arguments.to_string(),
            },
            thought_signature: None,
        }
    }

    struct CountingBeforeToolHook {
        count: Arc<AtomicU64>,
    }

    #[async_trait]
    impl BeforeToolCallHook for CountingBeforeToolHook {
        async fn before_tool_call(
            &self,
            _tool_call: &AgentToolCall,
            _state: &AgentState,
        ) -> BeforeToolCallResult {
            self.count.fetch_add(1, Ordering::SeqCst);
            BeforeToolCallResult::default()
        }
    }

    #[tokio::test]
    async fn coding_agent_builds_configurable_structured_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Always add focused tests.").unwrap();
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.system_prompt = SystemPromptConfig {
            custom_prompt: Some("CUSTOM_BASE".into()),
            append_prompt: Some("APPENDED_RULE".into()),
            guidelines: Vec::new(),
        };

        let coding_agent = CodingAgent::new(options);
        let state = coding_agent.agent.get_state().await;

        assert!(state
            .system_prompt
            .starts_with("CUSTOM_BASE\n\nAPPENDED_RULE"));
        assert!(state.system_prompt.contains("<project_context>"));
        assert!(state.system_prompt.contains("Always add focused tests."));
        assert!(state.system_prompt.contains("Current working directory:"));
    }

    #[tokio::test]
    async fn coding_agent_advertises_and_executes_discovered_skills() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".threadlane/skills/test-workflow");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-workflow\ndescription: Use for deterministic integration tests\n---\nBODY_SENTINEL",
        )
        .unwrap();

        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let state = coding_agent.agent.get_state().await;
        assert!(state.system_prompt.contains("`test-workflow`"));
        assert!(state
            .system_prompt
            .contains("Use for deterministic integration tests"));
        assert!(!state.system_prompt.contains("BODY_SENTINEL"));
        assert!(state.system_prompt.contains("- read_file:"));
        assert!(state.system_prompt.contains("- load_skill:"));

        let (chat, codex) = coding_agent.agent.loop_engine.build_api_payloads().await;
        assert!(chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["function"]["name"] == crate::skills::LOAD_SKILL_TOOL_NAME }));
        assert!(codex["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["name"] == crate::skills::LOAD_SKILL_TOOL_NAME }));
        assert!(codex["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["name"] == crate::plan::UPDATE_PLAN_TOOL_NAME }));

        let results = coding_agent
            .agent
            .loop_engine
            .execute_tools(&[provider_tool_call(
                "skill-call",
                crate::skills::LOAD_SKILL_TOOL_NAME,
                serde_json::json!({"name": "test-workflow"}),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error);
        assert!(results[0].content.contains("BODY_SENTINEL"));
    }

    #[tokio::test]
    async fn coding_agent_restores_the_session_plan() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let coding_agent = CodingAgent::new(options);

        let results = coding_agent
            .agent
            .loop_engine
            .execute_tools(&[provider_tool_call(
                "plan-call",
                crate::plan::UPDATE_PLAN_TOOL_NAME,
                serde_json::json!({
                    "plan": [{"step": "Verify", "status": "in_progress"}]
                }),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error);

        let mut restored_options = coding_agent_options(dir.path().to_path_buf());
        restored_options.session_file = Some(session_file);
        let restored = CodingAgent::new(restored_options);
        assert_eq!(restored.current_plan().items[0].step, "Verify");
    }

    #[tokio::test]
    async fn model_subagent_tool_returns_awaited_child_output() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join(".threadlane/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.md"),
            "---\nname: scout\ndescription: deterministic test scout\n---\nTest scout.",
        )
        .unwrap();

        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        coding_agent.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));
        let mut lifecycle_events = coding_agent.subscribe();
        let (chat, codex) = coding_agent.agent.loop_engine.build_api_payloads().await;
        assert!(chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "subagent"));
        assert!(codex["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "subagent"));

        let results = coding_agent
            .agent
            .loop_engine
            .execute_tools(&[provider_tool_call(
                "subagent-call",
                "subagent",
                serde_json::json!({
                    "tasks": [
                        {"agent": "scout", "task": "inspect the project"},
                        {"agent": "reviewer", "task": "review the project"}
                    ],
                    "parallel": true
                }),
            )])
            .await;
        let events = std::iter::from_fn(|| lifecycle_events.try_recv().ok()).collect::<Vec<_>>();
        let queued = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::SubagentQueued {
                    run_id,
                    task_index,
                    agent,
                    task,
                } => Some((*run_id, *task_index, agent.clone(), task.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let started = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::SubagentStarted { .. }))
            .count();
        let finished = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::SubagentFinished { .. }))
            .count();
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].0, queued[1].0);
        assert_eq!(queued[0].1, 0);
        assert_eq!(queued[1].1, 1);
        assert_eq!(started, 2);
        assert_eq!(finished, 2);
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error, "{}", results[0].content);
        assert!(results[0]
            .content
            .contains("test subagent result (test-model)"));
        assert!(!results[0].content.contains("Running 1 subagent task"));
    }

    #[tokio::test]
    async fn malformed_model_subagent_tool_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        let results = coding_agent
            .agent
            .loop_engine
            .execute_tools(&[provider_tool_call(
                "invalid-subagent-call",
                "subagent",
                serde_json::json!({
                    "tasks": [{"agent": "scout", "task": ""}],
                    "parallel": false
                }),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
    }

    #[tokio::test]
    async fn standalone_extension_command_runs_scheduled_agent_work() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = queue_command_wasm();
        let extension_dir = dir.path().join(".threadlane/extensions/queue_command_ext");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::write(extension_dir.join("extension.wasm"), wasm).unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        assert!(coding_agent.wasi_extensions.has_command("queue"));
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        let output = coding_agent.handle_input("/queue").await;

        assert_eq!(output.unwrap().unwrap(), "queued");
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::QueueMessage {
                content: "standalone queued work".into(),
                images: Vec::new(),
            }]
        );
    }

    #[tokio::test]
    async fn queued_follow_up_runs_through_the_agent_scheduler() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        coding_agent.work_handle().queue_follow_up_with_images(
            "interrupt the current turn",
            vec![ImageAttachment {
                display_name: "diagram.png".into(),
                data_url: "data:image/png;base64,AA==".into(),
            }],
        );
        coding_agent.run_scheduled_agent_work().await;

        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::QueueMessage {
                content: "interrupt the current turn".into(),
                images: vec![ImageAttachment {
                    display_name: "diagram.png".into(),
                    data_url: "data:image/png;base64,AA==".into(),
                }],
            }]
        );
    }

    #[tokio::test]
    async fn queued_follow_up_is_persisted_and_consumed_by_the_harness() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed);

        coding_agent
            .work_handle()
            .try_queue_follow_up_with_images("durable follow-up", Vec::new())
            .unwrap();
        let queued = JsonlStore::open(&session_file).unwrap();
        assert!(queued.records().iter().any(|record| matches!(
            record,
            HarnessRecord::QueueEnqueued {
                queue: QueueKind::FollowUp,
                ..
            }
        )));

        coding_agent.run_scheduled_agent_work().await;
        let consumed = JsonlStore::open(&session_file).unwrap();
        assert!(consumed
            .records()
            .iter()
            .any(|record| matches!(record, HarnessRecord::QueueConsumed { .. })));
        assert!(Reducer::reduce(&consumed)
            .unwrap()
            .lane("main")
            .unwrap()
            .queued
            .is_empty());
    }

    #[tokio::test]
    async fn queued_steer_is_persisted_and_consumed_by_the_harness() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        coding_agent
            .work_handle()
            .queue_steer_with_images("durable steer", Vec::new())
            .unwrap();
        let queued = JsonlStore::open(&session_file).unwrap();
        assert!(queued.records().iter().any(|record| matches!(
            record,
            HarnessRecord::QueueEnqueued {
                queue: QueueKind::Steer,
                ..
            }
        )));

        coding_agent.run_scheduled_agent_work().await;
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::SteerMessage {
                content: "durable steer".into(),
                images: Vec::new(),
            }]
        );
        let consumed = JsonlStore::open(&session_file).unwrap();
        assert!(consumed
            .records()
            .iter()
            .any(|record| matches!(record, HarnessRecord::QueueConsumed { .. })));
        assert!(Reducer::reduce(&consumed)
            .unwrap()
            .lane("main")
            .unwrap()
            .queued
            .is_empty());
    }

    #[tokio::test]
    async fn queued_next_run_is_persisted_and_consumed_by_the_harness() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.session_file = Some(session_file.clone());
        let mut coding_agent = CodingAgent::new(options);
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        coding_agent
            .work_handle()
            .queue_next_run_with_images("durable next run", Vec::new())
            .unwrap();
        let queued = JsonlStore::open(&session_file).unwrap();
        assert!(queued.records().iter().any(|record| matches!(
            record,
            HarnessRecord::QueueEnqueued {
                queue: QueueKind::NextRun,
                ..
            }
        )));

        coding_agent.run_scheduled_agent_work().await;
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::NextRunMessage {
                content: "durable next run".into(),
                images: Vec::new(),
            }]
        );
        let consumed = JsonlStore::open(&session_file).unwrap();
        assert!(Reducer::reduce(&consumed)
            .unwrap()
            .lane("main")
            .unwrap()
            .queued
            .is_empty());
    }

    #[tokio::test]
    async fn generic_agent_run_inherits_parent_current_model_for_tasks_without_model() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join(".threadlane/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.md"),
            "---\nname: scout\ndescription: deterministic test scout\n---\nTest scout.",
        )
        .unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.set_subagent_work_observer(observed.clone());
        coding_agent.agent.loop_engine.state.lock().await.model = "changed-model".into();

        let output = coding_agent
            .handle_input("/subagent inspect the project")
            .await;

        assert!(output
            .unwrap()
            .unwrap()
            .contains("test subagent result (changed-model)"));
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::QueueMessage {
                content: "test subagent follow-up".into(),
                images: Vec::new(),
            }]
        );
    }

    #[tokio::test]
    async fn dynamic_subagent_spawns_without_predefined_agent_file() {
        let dir = tempfile::tempdir().unwrap();
        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        coding_agent.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));

        let results = coding_agent
            .agent
            .loop_engine
            .execute_tools(&[provider_tool_call(
                "dynamic-subagent-call",
                "subagent",
                serde_json::json!({
                    "tasks": [{
                        "agent": "custom_architect",
                        "task": "design architecture",
                        "instructions": "You are a custom architect.",
                        "tools": ["read_file"]
                    }],
                    "parallel": false
                }),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error, "{}", results[0].content);
        assert!(results[0].content.contains("test subagent result"));
    }

    #[test]
    fn generic_tool_policy_state_restores_by_session() {
        let dir = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(dir.path(), "session-a");
        manager
            .set_host_state("tools.policy", Value::String("read_only".into()))
            .unwrap();

        let restored = WasiExtensionManager::for_project_session(dir.path(), "session-a");
        assert_eq!(
            restored.host_state("tools.policy"),
            Some(Value::String("read_only".into()))
        );
    }

    #[test]
    fn tool_policy_is_unchanged_when_host_state_persistence_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".threadlane"), "not a directory").unwrap();
        let policy = Arc::new(tokio::sync::Mutex::new(ToolPolicy::FullAccess));
        let tools = HostCapabilityHandler {
            tool_policy: Some(policy.clone()),
            extensions: Arc::new(WasiExtensionManager::for_project_session(
                dir.path(),
                "session-a",
            )),
            persist_tool_policy: true,
            ..handler("tools", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: BROKER_API_VERSION,
            capability: "tools".into(),
            operation: "set_policy".into(),
            arguments: serde_json::json!({"policy": "read_only"}),
        };

        assert_eq!(tools.handle(&request).unwrap_err().code, "host_error");
        assert_eq!(*policy.try_lock().unwrap(), ToolPolicy::FullAccess);
    }

    #[test]
    fn filesystem_rejects_paths_outside_work_dir() {
        let dir = tempfile::tempdir().unwrap();
        let request = BrokerRequest {
            api_version: 2,
            capability: "fs".into(),
            operation: "read_text".into(),
            arguments: serde_json::json!({"path": "../outside"}),
        };
        let error = handler("fs", dir.path().to_path_buf())
            .handle(&request)
            .unwrap_err();
        assert_eq!(error.code, "invalid_argument");
    }

    struct RecordingBrokerHandler {
        operations: Arc<Mutex<Vec<String>>>,
    }

    impl CapabilityHandler for RecordingBrokerHandler {
        fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
            self.operations
                .lock()
                .unwrap()
                .push(request.operation.clone());
            if request.operation == "fail" {
                Err(BrokerError {
                    code: "test_error".into(),
                    message: "expected test failure".into(),
                })
            } else {
                Ok(Value::Null)
            }
        }
    }

    #[tokio::test]
    async fn tool_broker_requests_dispatch_in_order_and_isolate_errors() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let dispatcher = Arc::new(dispatcher);
        let requests = ["first", "fail", "last"]
            .into_iter()
            .map(|operation| crate::extension_broker::HostBrokerRequest {
                request: BrokerRequest {
                    api_version: BROKER_API_VERSION,
                    capability: "tools".into(),
                    operation: operation.into(),
                    arguments: Value::Null,
                },
                invoking_extension: "tool-ext".into(),
            })
            .collect();

        let extensions = WasiExtensionManager::new();
        dispatch_hook_requests_isolated(&dispatcher, &extensions, requests, "test broker error")
            .await;

        assert_eq!(*operations.lock().unwrap(), vec!["first", "fail", "last"]);
    }

    #[test]
    fn wasi_extension_response_defaults_continuation_to_false() {
        let response: WasiExtensionResponse =
            serde_json::from_value(serde_json::json!({"message": "legacy"})).unwrap();

        assert!(!response.continue_after_broker);
    }

    #[tokio::test]
    async fn wasi_tool_continuation_post_processes_broker_operation_errors() {
        let extension =
            WasiExtension::load_from_bytes(broker_tool_wasm("fail", true, true)).unwrap();
        let extensions = WasiExtensionManager::new();
        extensions.register_extension(extension).unwrap();
        let extensions = Arc::new(extensions);
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let executor = BrokerAwareWasiToolExecutor {
            extensions: extensions.clone(),
            broker_dispatcher: Arc::new(dispatcher),
        };

        let output = executor
            .execute_tool(CONTINUATION_TOOL_NAME, CONTINUATION_TOOL_ARGS)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(output, "post-processed broker response");
        assert_eq!(*operations.lock().unwrap(), vec!["fail"]);
        assert!(extensions
            .drain_events_for(CONTINUATION_EXTENSION_NAME)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn wasi_tool_without_continuation_preserves_queued_broker_results() {
        let extension =
            WasiExtension::load_from_bytes(broker_tool_wasm("fail", false, false)).unwrap();
        let extensions = WasiExtensionManager::new();
        extensions.register_extension(extension).unwrap();
        let extensions = Arc::new(extensions);
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let executor = BrokerAwareWasiToolExecutor {
            extensions: extensions.clone(),
            broker_dispatcher: Arc::new(dispatcher),
        };

        let error = executor
            .execute_tool(CONTINUATION_TOOL_NAME, CONTINUATION_TOOL_ARGS)
            .await
            .unwrap()
            .unwrap_err();

        assert_eq!(error, "expected test failure");
        assert_eq!(*operations.lock().unwrap(), vec!["fail"]);
        let events = extensions
            .drain_events_for(CONTINUATION_EXTENSION_NAME)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["error"]["code"], "test_error");
    }

    #[tokio::test]
    async fn wasi_tool_continuation_has_an_actionable_round_limit() {
        let extension =
            WasiExtension::load_from_bytes(broker_tool_wasm("loop", true, false)).unwrap();
        let extensions = WasiExtensionManager::new();
        extensions.register_extension(extension).unwrap();
        let extensions = Arc::new(extensions);
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let executor = BrokerAwareWasiToolExecutor {
            extensions,
            broker_dispatcher: Arc::new(dispatcher),
        };

        let error = executor
            .execute_tool(CONTINUATION_TOOL_NAME, CONTINUATION_TOOL_ARGS)
            .await
            .unwrap()
            .unwrap_err();

        assert_eq!(
            operations.lock().unwrap().len(),
            MAX_BROKER_CONTINUATION_ROUNDS
        );
        assert!(error.contains(CONTINUATION_TOOL_NAME));
        assert!(error.contains(&format!("{MAX_BROKER_CONTINUATION_ROUNDS} rounds")));
        assert!(error.contains("broker_response"));
    }

    #[test]
    fn process_run_limits_preserve_defaults_and_apply_hard_caps() {
        assert_eq!(
            process_run_limits(&serde_json::json!({})).unwrap(),
            ProcessRunLimits {
                timeout: CAPABILITY_TIMEOUT,
                max_output_bytes: MAX_CAPABILITY_BUFFER_BYTES,
            }
        );
        assert_eq!(
            process_run_limits(&serde_json::json!({
                "timeout_ms": 1_234,
                "max_output_bytes": 4_096,
            }))
            .unwrap(),
            ProcessRunLimits {
                timeout: Duration::from_millis(1_234),
                max_output_bytes: 4_096,
            }
        );
        assert_eq!(
            process_run_limits(&serde_json::json!({
                "timeout_ms": MAX_PROCESS_TIMEOUT_MS + 1,
                "max_output_bytes": MAX_PROCESS_OUTPUT_BYTES as u64 + 1,
            }))
            .unwrap(),
            ProcessRunLimits {
                timeout: Duration::from_millis(MAX_PROCESS_TIMEOUT_MS),
                max_output_bytes: MAX_PROCESS_OUTPUT_BYTES,
            }
        );
        assert_eq!(
            process_run_limits(&serde_json::json!({"timeout_ms": 0}))
                .unwrap_err()
                .code,
            "invalid_argument"
        );
        assert_eq!(
            process_run_limits(&serde_json::json!({"max_output_bytes": "1024"}))
                .unwrap_err()
                .code,
            "invalid_argument"
        );
    }

    #[tokio::test]
    async fn process_pipes_output_and_timeout_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let mut request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "run".into(),
            arguments: serde_json::json!({
                "program": "sh",
                "args": ["-c", "printf stdout; printf stderr >&2"]
            }),
        };
        let output = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();
        let output: Value = serde_json::from_str(output["message"].as_str().unwrap()).unwrap();
        assert_eq!(output["stdout"], "stdout");
        assert_eq!(output["stderr"], "stderr");

        request.arguments = serde_json::json!({
            "program": "sh",
            "args": ["-c", "sleep 10"],
            "timeout_ms": 25
        });
        let started = Instant::now();
        let error = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "timeout");
        assert!(started.elapsed() < StdDuration::from_secs(1));
    }

    #[tokio::test]
    async fn process_output_is_bounded_before_buffering() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "run".into(),
            arguments: serde_json::json!({
                "program": "sh",
                "args": ["-c", format!("head -c {} /dev/zero", MAX_CAPABILITY_BUFFER_BYTES + 1)]
            }),
        };
        let error = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "process_output_too_large");

        let request = BrokerRequest {
            arguments: serde_json::json!({
                "program": "sh",
                "args": ["-c", "printf 123456789"],
                "max_output_bytes": 8
            }),
            ..request
        };
        let error = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "process_output_too_large");
        assert!(error.message.contains("8-byte buffer limit"));
    }

    #[tokio::test]
    async fn managed_process_round_trips_one_content_length_message() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "spawn".into(),
            arguments: serde_json::json!({
                "name": "echo",
                "program": "cat",
                "args": []
            }),
        };
        process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();

        let request = BrokerRequest {
            operation: "send".into(),
            arguments: serde_json::json!({
                "name": "echo",
                "data": "Content-Length: 2\r\n\r\nok\n"
            }),
            ..request
        };
        process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();

        let request = BrokerRequest {
            operation: "recv".into(),
            arguments: serde_json::json!({
                "name": "echo",
                "framing": "content-length",
                "timeout_ms": 1_000
            }),
            ..request
        };
        let output = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();
        let output: Value = serde_json::from_str(output["message"].as_str().unwrap()).unwrap();
        assert_eq!(output, serde_json::json!({"data": "ok", "eof": false}));
    }

    #[tokio::test]
    async fn managed_processes_are_private_to_the_invoking_extension() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "spawn".into(),
            arguments: serde_json::json!({"name": "private", "program": "cat", "args": []}),
        };
        process
            .handle_for_extension_async(&request, "owner")
            .await
            .unwrap();
        let output = process
            .handle_for_extension_async(
                &BrokerRequest {
                    operation: "status".into(),
                    arguments: serde_json::json!({"name": "private"}),
                    ..request
                },
                "other",
            )
            .await
            .unwrap();
        let output: Value = serde_json::from_str(output["message"].as_str().unwrap()).unwrap();
        assert_eq!(output["processes"], serde_json::json!([]));
    }

    #[test]
    fn filesystem_rename_stays_inside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.rs"), "fn old() {}").unwrap();
        let fs = handler("fs", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "fs".into(),
            operation: "rename".into(),
            arguments: serde_json::json!({"from": "old.rs", "to": "new.rs"}),
        };

        fs.handle(&request).unwrap();

        assert!(!dir.path().join("old.rs").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.rs")).unwrap(),
            "fn old() {}"
        );
    }

    #[tokio::test]
    async fn network_response_is_bounded_before_buffering() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
            response.resize(MAX_CAPABILITY_BUFFER_BYTES + 1, b'x');
            tokio::io::AsyncWriteExt::write_all(&mut socket, &response)
                .await
                .unwrap();
        });
        let network = HostCapabilityHandler {
            allowed_hosts: Arc::new(HashSet::from(["127.0.0.1".to_string()])),
            ..handler("network", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: 2,
            capability: "network".into(),
            operation: "http".into(),
            arguments: serde_json::json!({
                "url": format!("http://127.0.0.1:{port}/"),
                "method": "GET",
                "body": ""
            }),
        };
        let error = network
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "network_response_too_large");
    }

    #[tokio::test]
    async fn network_io_timeout_is_bounded_after_connect() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let network = HostCapabilityHandler {
            allowed_hosts: Arc::new(HashSet::from(["127.0.0.1".to_string()])),
            ..handler("network", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: 2,
            capability: "network".into(),
            operation: "http".into(),
            arguments: serde_json::json!({
                "url": format!("http://127.0.0.1:{port}/"),
                "method": "GET",
                "body": ""
            }),
        };
        let started = Instant::now();
        let error = network
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "timeout");
        assert!(started.elapsed() < StdDuration::from_secs(4));
    }
}
