use super::harness::{harness_cancellation_state, CodingSessionHarness};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use threadlane_runtime::harness::{
    JsonlStore, OperationOutcome, ProvisionedEntry, Record as HarnessRecord, Reducer,
};
use threadlane_runtime::{AgentEvent, AgentMessage};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunTask {
    pub(crate) agent: String,
    pub(crate) task: String,
    pub(crate) instructions: Option<String>,
    pub(crate) tools: Option<Vec<String>>,
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) context_refs: Vec<String>,
}

pub(crate) fn recover_v2_subagent_records(
    session_file: &Path,
) -> Result<Vec<HarnessRecord>, String> {
    let store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
    let mut records: Vec<HarnessRecord> = store
        .records()
        .iter()
        .filter(|r| r.lane() != "main")
        .cloned()
        .collect();
    let open_runs: HashMap<_, _> = records
        .iter()
        .filter_map(|record| match record {
            HarnessRecord::OperationStarted { lane, id, .. } => Some((lane.clone(), id.clone())),
            _ => None,
        })
        .collect();
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
            records.push(HarnessRecord::WriteDeferred {
                id: entry.id.clone(),
                seq: entry.seq,
                lane: entry.lane.clone(),
                timestamp: entry.timestamp,
                run_id: run_id.clone(),
                target: ProvisionedEntry {
                    id: entry.id.clone(),
                    surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                    parent_id: entry.parent_id.clone(),
                    message: entry.message.clone(),
                },
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
                    HarnessRecord::OperationStarted { id, lane, .. } if id == &run_id => {
                        Some(lane.clone())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let task = messages.iter().rev().find_map(|message| match message {
                AgentMessage::User { content } => Some(content.clone()),
                _ => None,
            });
            if let Some(_task) = task {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                records.push(HarnessRecord::StepAttempt {
                    id: format!("task-attempt-{run_id}-recovered"),
                    seq: records.iter().map(HarnessRecord::seq).max().unwrap_or(0) + 1,
                    lane,
                    timestamp: now_ms,
                    run_id,
                    attempt: 1,
                    result_entry_id: String::new(),
                    compaction_reason: None,
                });
            }
        }
    }
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
    pub(crate) fn new(
        harness_session_file: Option<PathBuf>,
        event_tx: broadcast::Sender<AgentEvent>,
    ) -> Self {
        Self {
            state: Arc::default(),
            harness_session_file,
            event_tx,
        }
    }

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

    pub(crate) fn clear_cancellation_guard(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cancellation_guard = None;
        }
        if let Some(path) = self.harness_session_file.as_deref() {
            harness_cancellation_state(path).store(false, Ordering::SeqCst);
        }
    }

    pub(crate) fn cancel(&self) -> Result<(), String> {
        let durable_run_id = if let Some(path) = self.harness_session_file.as_deref() {
            let mut journal = CodingSessionHarness::open(path)?;
            journal.request_abort()?
        } else {
            None
        };
        let handle = {
            let mut state = self.state.lock().map_err(|error| error.to_string())?;
            state.active.take().map(|active| active.handle)
        };
        let acknowledged = handle.is_some();
        if let Some(handle) = handle {
            handle.abort();
        }
        if let (Some(path), Some(run_id)) = (
            self.harness_session_file.as_deref(),
            durable_run_id.as_deref(),
        ) {
            CodingSessionHarness::open(path)?.observe_abort_signal(run_id, acknowledged)?;
        }
        let _ = self.event_tx.send(AgentEvent::AgentError {
            error: "Generation cancelled".into(),
        });
        Ok(())
    }
}

pub fn cancel_open_subagent_operations(session_file: &Path) -> Result<(), String> {
    if session_file.exists() {
        let mut journal = CodingSessionHarness::open(session_file)?;
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
