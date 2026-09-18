use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::context_snapshots::{
    compacted_context_snapshot_index_for_sources, is_local_path, read_file_request,
};
use threadlane_permission::PermissionTraceEvent;
use threadlane_compaction::{
    build_checkpoint_omitting_tool_outputs, compact_for_budget, estimate_request_tokens,
    CompactionParams, PreparedCompaction,
};
pub use threadlane_runtime::harness::Record as HarnessRecord;
use threadlane_runtime::harness::{
    AbortInitiator, AbortObservation, AbortTarget, AgentHarness, BoundedText, CapabilitySnapshot,
    CompactionReason, ContextSnapshotLoadOutcome, DeferredResolution, Entry as HarnessEntry,
    ErrorCategory, HarnessEventHub, HookRegistry, JsonlStore,
    OperationOutcome, PromptSnapshot, ProviderErrorSummary, ProviderOutcome, ProvisionedEntry,
    QueueKind, Reducer, RetryPolicy, SessionIdGenerator, SessionStore, Snapshot,
    SubagentLifecyclePhase, ToolExecutionOutcome, ToolExecutionPhase,
    ToolReplaySafety as HarnessToolReplaySafety, ToolResult as HarnessToolResult, ToolSpec,
    TraceString,
};
use threadlane_context::{context_budget, BudgetConfig, ContextBudget};
use threadlane_protocol::{
    AgentMessage, AgentToolResult, ImageAttachment, ReasoningEffort, TokenUsage,
};
use threadlane_runtime::{
    AgentConfig, ProviderBoundaryRequest, ProviderBoundaryResult, ProviderTraceEvent,
    ToolExecutionTraceEvent,
};

use threadlane_runtime::harness::OperationIntent;
#[cfg(test)]
use threadlane_runtime::harness::HookContext;


mod assistant;
mod boundary;
mod cancel;
mod compaction;
mod context;
mod journal;
mod lanes;
mod observation;
mod records;
mod replay;
mod runs;
mod tools;
#[cfg(test)]
static LAST_PATH_OPERATION_THREAD: std::sync::Mutex<Option<std::thread::ThreadId>> =
    std::sync::Mutex::new(None);
static NEXT_CONTEXT_SNAPSHOT_LOAD_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
fn last_path_operation_thread() -> Option<std::thread::ThreadId> {
    *LAST_PATH_OPERATION_THREAD.lock().ok()?
}

#[derive(Clone)]
struct HarnessSessionEntry {
    hub: HarnessEventHub,
    hooks: HookRegistry,
    cancellation: Arc<AtomicBool>,
}

fn harness_session_entry(path: &Path) -> HarnessSessionEntry {
    static SESSIONS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, HarnessSessionEntry>>> =
        std::sync::OnceLock::new();
    let sessions = SESSIONS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut sessions = sessions.lock().unwrap_or_else(|error| error.into_inner());
    sessions
        .entry(path.to_path_buf())
        .or_insert_with(|| HarnessSessionEntry {
            hub: HarnessEventHub::new(256),
            hooks: HookRegistry::default(),
            cancellation: Arc::new(AtomicBool::new(false)),
        })
        .clone()
}

fn harness_event_hub(path: &Path) -> HarnessEventHub {
    harness_session_entry(path).hub
}

fn harness_hook_registry(path: &Path) -> HookRegistry {
    harness_session_entry(path).hooks
}

pub(crate) fn harness_cancellation_state(path: &Path) -> Arc<AtomicBool> {
    harness_session_entry(path).cancellation
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentLaneIdentity {
    pub(crate) lane_name: String,
    pub(crate) run_id: String,
    pub(crate) source_leaf_id: Option<String>,
    pub(crate) started_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartedSubagentLane {
    pub(crate) identity: SubagentLaneIdentity,
    accepted: AcceptedRun,
}

#[derive(Debug)]
pub struct SubagentStartError {
    pub(crate) identity: Option<SubagentLaneIdentity>,
    pub(crate) error: String,
}

pub use threadlane_runtime::AcceptedRun;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterruptedSubagentRecoveryState {
    Pending,
    Complete,
}

/// Owns the durable session store, the `main` lane handle, event hub, hook
/// registry, cancellation state, and a subscription for event projection.
/// Every foreground operation enters the harness through this adapter;
/// there is no second persistence path.
pub struct CodingSessionHarness {
    pub(crate) store: AgentHarness<JsonlStore>,
    pub(crate) main_lane_name: String,
    pub(crate) events: HarnessEventHub,
    pub(crate) hooks: HookRegistry,
    pub(crate) cancellation: Arc<AtomicBool>,
}

fn boundary_result(
    messages: Vec<AgentMessage>,
    budget: ContextBudget,
    compaction_generation: u64,
    provisional_estimated_tokens: Option<usize>,
    provider_attempt: u32,
    provider_request_id: String,
) -> ProviderBoundaryResult {
    ProviderBoundaryResult {
        messages,
        context_limit: budget.limit,
        context_limit_is_estimate: budget.limit_is_estimate,
        compaction_generation,
        provisional_estimated_tokens,
        provider_attempt: Some(provider_attempt),
        provider_request_id: Some(provider_request_id),
    }
}
impl CodingSessionHarness {
    // ── Construction ──────────────────────────────────────────────────

    /// Open or create the JSONL session at `path` and build a canonical
    /// harness adapter.
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| error.to_string())?;
        }
        let events = harness_event_hub(path);
        let hooks = harness_hook_registry(path);
        let cancellation = harness_cancellation_state(path);
        let store = JsonlStore::open(path)
            .map(|store| AgentHarness::with_events_and_hooks(store, events.clone(), hooks.clone()))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            store,
            main_lane_name: "main".into(),
            events,
            hooks,
            cancellation,
        })
    }

    /// Opens a short-lived journal for one path-scoped operation. `open`
    /// already parses and reduces the full history once; downstream appends
    /// re-validate freshness under the writer gate via `is_fresh`, so no
    /// second eager reload happens here.
    fn with_path<T>(
        path: &Path,
        operation: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        #[cfg(test)]
        if let Ok(mut thread) = LAST_PATH_OPERATION_THREAD.lock() {
            *thread = Some(std::thread::current().id());
        }
        let mut journal = Self::open(path)?;
        operation(&mut journal)
    }

    /// Reloads the durable store only when another writer has appended
    /// (cheap file-length probe), instead of unconditionally reparsing.
    pub(crate) fn ensure_fresh(&mut self) -> Result<(), String> {
        self.store
            .store_mut()
            .ensure_fresh()
            .map_err(|error| error.to_string())
    }

    /// Append a durable fact through the canonical session harness adapter.
    pub fn append_fact_to_path(
        path: &Path,
        lane: &str,
        key: &str,
        value: &str,
        run_id: Option<&str>,
    ) -> Result<(), String> {
        Self::with_path(path, |journal| {
            journal
                .store
                .store_mut()
                .append_fact(lane, key, value, run_id)
                .map_err(|error| error.to_string())
        })
    }








    // ── Cancellation ──────────────────────────────────────────────────


}

// ── Helpers ───────────────────────────────────────────────────────────

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn harness_next_seq(store: &JsonlStore) -> u64 {
    store.next_sequence()
}
#[cfg(test)]
mod tests;
