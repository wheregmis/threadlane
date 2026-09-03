use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::context_snapshots::{
    file_sha256, is_local_path, read_file_request, render_compacted_context_index,
};
use crate::permission::PermissionTraceEvent;
use threadlane_runtime::compaction::{
    compact_for_budget, estimate_request_tokens, PreparedCompaction,
};
pub use threadlane_runtime::harness::Record as HarnessRecord;
use threadlane_runtime::harness::{
    AbortInitiator, AbortObservation, AbortTarget, AgentHarness, BoundedText, CapabilitySnapshot,
    CompactionReason, ContextSnapshotLoadOutcome, DeferredResolution, Entry as HarnessEntry,
    ErrorCategory, HarnessEventHub, HookContext, HookKind, HookRegistry, JsonlStore,
    OperationOutcome, PromptSnapshot, ProviderErrorSummary, ProviderOutcome, ProvisionedEntry,
    QueueKind, Reducer, RetryPolicy, SessionIdGenerator, SessionStore, Snapshot,
    SubagentLifecyclePhase, ToolExecutionOutcome, ToolExecutionPhase,
    ToolReplaySafety as HarnessToolReplaySafety, ToolResult as HarnessToolResult, ToolSpec,
    TraceString,
};
use threadlane_runtime::model_metadata::{context_budget, ContextBudget};
use threadlane_runtime::{
    AgentConfig, AgentMessage, AgentToolResult, ImageAttachment, ProviderBoundaryRequest,
    ProviderBoundaryResult, ProviderTraceEvent, ReasoningEffort, TokenUsage,
    ToolExecutionTraceEvent,
};

use threadlane_runtime::harness::{EventError, HarnessEvent, OperationIntent, Subscription};

#[cfg(test)]
static LAST_PATH_OPERATION_THREAD: std::sync::Mutex<Option<std::thread::ThreadId>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn last_path_operation_thread() -> Option<std::thread::ThreadId> {
    *LAST_PATH_OPERATION_THREAD.lock().ok()?
}

pub struct HarnessWatch {
    pub(crate) hub: HarnessEventHub,
    pub(crate) subscription: Subscription,
}

impl HarnessWatch {
    pub fn snapshot(&self) -> &Snapshot {
        &self.subscription.snapshot
    }

    pub(crate) async fn wait(&mut self) -> Result<Vec<HarnessEvent>, EventError> {
        self.hub.wait(&mut self.subscription).await
    }
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
pub(crate) struct SubagentLaneIdentity {
    pub(crate) lane_name: String,
    pub(crate) run_id: String,
    pub(crate) source_leaf_id: Option<String>,
    pub(crate) started_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartedSubagentLane {
    pub(crate) identity: SubagentLaneIdentity,
    pub(crate) accepted: AcceptedRun,
}

#[derive(Debug)]
pub(crate) struct SubagentStartError {
    pub(crate) identity: Option<SubagentLaneIdentity>,
    pub(crate) error: String,
}

pub(crate) use threadlane_runtime::AcceptedRun;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterruptedSubagentRecoveryState {
    Pending,
    Complete,
}

/// Owns the durable session store, the `main` lane handle, event hub, hook
/// registry, cancellation state, and a subscription for event projection.
/// Every foreground operation enters the harness through this adapter;
/// there is no second persistence path.
#[allow(dead_code)]
pub struct CodingSessionHarness {
    pub(crate) store: AgentHarness<JsonlStore>,
    pub(crate) session_path: PathBuf,
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
#[allow(dead_code)]
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
            session_path: path.to_path_buf(),
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

    fn append_record_to_path(path: &Path, record: HarnessRecord) -> Result<(), String> {
        Self::with_path(path, |journal| {
            journal
                .store
                .append_record_gated(record)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())
        })
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

    pub(crate) fn append_message_to_path(path: &Path, message: AgentMessage) -> Result<(), String> {
        Self::with_path(path, |journal| journal.append_message(message).map(|_| ()))
    }

    pub(crate) fn index_read_snapshot(
        &mut self,
        run_id: &str,
        work_dir: &Path,
        tool_call_id: &str,
        source_entry_id: &str,
        output_chars: usize,
    ) -> Result<Option<String>, String> {
        self.ensure_fresh()?;
        let Some((effective_args, result_entry_id)) =
            self.store.records().iter().find_map(|record| match record {
                HarnessRecord::ToolStarted {
                    run_id: record_run_id,
                    tool_call_id: record_call_id,
                    tool_name,
                    effective_args,
                    result_entry_id,
                    ..
                } if record_run_id == run_id
                    && record_call_id == tool_call_id
                    && tool_name == "read_file" =>
                {
                    Some((effective_args, result_entry_id))
                }
                _ => None,
            })
        else {
            return Ok(None);
        };
        if result_entry_id != source_entry_id {
            return Ok(None);
        }
        let Some((path, start_line, end_line)) = read_file_request(effective_args) else {
            return Ok(None);
        };
        if !is_local_path(path) {
            return Ok(None);
        }
        let Some(entry) = self
            .store
            .entries()
            .iter()
            .find(|entry| entry.id == source_entry_id)
        else {
            return Ok(None);
        };
        if !matches!(
            &entry.message,
            AgentMessage::Tool { tool_call_id: entry_call_id, name, is_error: false, .. }
                if entry_call_id == tool_call_id && name == "read_file"
        ) {
            return Ok(None);
        }
        let canonical_path = threadlane_tools::validate_path_in_workspace(path, work_dir)?;
        let canonical_work_dir = work_dir.canonicalize().map_err(|error| error.to_string())?;
        let relative_path = canonical_path
            .strip_prefix(&canonical_work_dir)
            .map_err(|_| {
                format!(
                    "read path '{}' is outside workspace",
                    canonical_path.display()
                )
            })?
            .to_string_lossy()
            .into_owned();
        let context_id = format!("ctx-{source_entry_id}");
        if self.context_snapshots("main").iter().any(|snapshot| {
            snapshot.context_id == context_id
                && snapshot.source_run_id == run_id
                && snapshot.source_tool_call_id == tool_call_id
                && snapshot.source_entry_id == source_entry_id
        }) {
            return Ok(Some(context_id));
        }
        let snapshot = threadlane_runtime::harness::ContextSnapshot {
            context_id: context_id.clone(),
            source_lane: "main".into(),
            source_run_id: run_id.into(),
            source_tool_call_id: tool_call_id.into(),
            source_entry_id: source_entry_id.into(),
            path: relative_path,
            start_line,
            end_line,
            file_sha256: file_sha256(&canonical_path)?,
            output_chars,
            captured_at: timestamp(),
        };
        self.store
            .append_record_gated(HarnessRecord::ContextSnapshotIndexed {
                id: format!("context-snapshot-{context_id}"),
                seq: self.next_seq(),
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                snapshot,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(Some(context_id))
    }

    pub(crate) fn context_snapshots(
        &self,
        lane: &str,
    ) -> Vec<threadlane_runtime::harness::ContextSnapshot> {
        Reducer::reduce(self.store.store())
            .ok()
            .and_then(|state| state.lane(lane).map(|lane| lane.context_snapshots.clone()))
            .unwrap_or_default()
    }

    pub(crate) fn record_context_snapshot_load_to_path(
        path: &Path,
        context_id: &str,
        source_lane: &str,
        current_digest: Option<TraceString>,
        outcome: ContextSnapshotLoadOutcome,
    ) -> Result<(), String> {
        Self::with_path(path, |journal| {
            journal.ensure_fresh()?;
            let run_id = Reducer::reduce(journal.store.store())
                .ok()
                .and_then(|state| {
                    state
                        .lane("main")
                        .and_then(|lane| lane.open_operation.clone())
                })
                .unwrap_or_else(|| "context-load".into());
            let seq = journal.next_seq();
            journal
                .store
                .append_record_gated(HarnessRecord::ContextSnapshotLoaded {
                    id: format!("context-snapshot-load-{seq}"),
                    seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id,
                    context_id: context_id.into(),
                    source_lane: source_lane.into(),
                    current_digest,
                    outcome,
                })
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())
        })
    }

    pub(crate) fn capture_run_context(
        &mut self,
        run_id: &str,
        lane: &str,
        model: String,
        provider: String,
        reasoning_effort: ReasoningEffort,
        prompt_cache_enabled: bool,
        work_dir: String,
        system_prompt: PromptSnapshot,
        tool_schema_sha256: String,
        enabled_tool_names: Vec<String>,
        capabilities: Vec<String>,
        capability_sha256: Option<String>,
        prompt_template_ids: Vec<String>,
        git_head: Option<String>,
        context_window_limit: Option<usize>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let trace = |value: String| TraceString::new(value);
        let record = HarnessRecord::RunContextCaptured {
            id: format!("run-context-{run_id}"),
            seq: harness_next_seq(self.store.store()),
            lane: lane.into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            attempt: None,
            model: trace(model)?,
            provider: trace(provider)?,
            reasoning_effort,
            prompt_cache_enabled,
            work_dir: trace(work_dir)?,
            system_prompt,
            tool_schema_sha256: trace(tool_schema_sha256)?,
            enabled_tool_names: enabled_tool_names
                .into_iter()
                .take(256)
                .map(TraceString::new)
                .collect::<Result<Vec<_>, _>>()?,
            capabilities: CapabilitySnapshot {
                capabilities: capabilities
                    .into_iter()
                    .take(256)
                    .map(TraceString::new)
                    .collect::<Result<Vec<_>, _>>()?,
                fingerprint: capability_sha256.map(TraceString::new).transpose()?,
            },
            prompt_template_ids: prompt_template_ids
                .into_iter()
                .take(256)
                .map(TraceString::new)
                .collect::<Result<Vec<_>, _>>()?,
            git_head: git_head.map(TraceString::new).transpose()?,
            context_window_limit,
            route_defaults: None,
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub fn model_context(
        &self,
        lane: &str,
    ) -> Result<threadlane_runtime::harness::ModelContextProjection, String> {
        self.store
            .store()
            .model_context(lane)
            .map_err(|error| error.to_string())
    }

    pub fn prepare_provider_boundary(
        &mut self,
        run_id: &str,
        request: ProviderBoundaryRequest,
        config: &AgentConfig,
    ) -> Result<ProviderBoundaryResult, String> {
        self.ensure_fresh()?;
        // No provider boundary may proceed after cancellation, even when the
        // already-compacted context is below the adaptive trigger. Once a
        // checkpoint procedure starts, it is driven atomically to completion.
        if self.cancellation.load(Ordering::SeqCst) {
            return Err("context preparation cancelled".into());
        }
        let budget = context_budget(&request.model, config);
        let provider_attempt = self
            .store
            .store()
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestStarted {
                    run_id: record_run_id,
                    attempt,
                    ..
                } if record_run_id == run_id => Some(*attempt),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let provider_request_id = format!("provider-request-{run_id}-{provider_attempt}");
        let mut current = self.model_context("main")?.messages();
        let pre_tokens =
            estimate_request_tokens(&current, request.tool_schema_json.as_deref(), config);
        if pre_tokens < budget.trigger_tokens && !request.overflow_recovery {
            return Ok(boundary_result(
                current,
                budget,
                self.compaction_generation(),
                None,
                provider_attempt,
                provider_request_id,
            ));
        }
        let reason = if request.overflow_recovery {
            CompactionReason::OverflowRecovery
        } else {
            CompactionReason::AdaptiveBudget
        };
        let targets = [
            budget.retained_tail_tokens,
            budget.strict_retained_tail_tokens,
        ];
        for (index, target) in targets.into_iter().enumerate() {
            let Some(prepared) = compact_for_budget(
                &current,
                request.tool_schema_json.as_deref(),
                target,
                config,
            ) else {
                return Err("context preparation could not drop historical messages".into());
            };
            self.commit_prepared_compaction(
                run_id,
                &request.model,
                request.tool_schema_json.as_deref(),
                config,
                budget,
                reason,
                prepared,
            )?;
            current = self.model_context("main")?.messages();
            let post_tokens =
                estimate_request_tokens(&current, request.tool_schema_json.as_deref(), config);
            if post_tokens < budget.trigger_tokens {
                return Ok(boundary_result(
                    current,
                    budget,
                    self.compaction_generation(),
                    Some(post_tokens),
                    provider_attempt,
                    provider_request_id,
                ));
            }
            if index == 1 {
                return Err(format!(
                    "context remains above budget after strict compaction: {post_tokens}/{}",
                    budget.trigger_tokens,
                ));
            }
            self.ensure_fresh()?;
        }
        unreachable!()
    }

    fn compaction_generation(&self) -> u64 {
        self.store
            .store()
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    lane, generation, ..
                } if lane == "main" => Some(*generation),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn checkpoint_open_run_compaction(
        &mut self,
        run_id: &str,
        summary: &str,
        reason: CompactionReason,
    ) -> Result<(), String> {
        self.stage_open_run_compaction(run_id, summary, reason)?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn stage_open_run_compaction(
        &mut self,
        run_id: &str,
        summary: &str,
        reason: CompactionReason,
    ) -> Result<(), String> {
        self.store
            .checkpoint_open_run_compaction("main", run_id, summary, reason)
            .map_err(|error| error.to_string())
    }

    fn commit_prepared_compaction(
        &mut self,
        parent_run_id: &str,
        model: &str,
        tool_schema_json: Option<&str>,
        config: &AgentConfig,
        budget: ContextBudget,
        reason: CompactionReason,
        prepared: PreparedCompaction,
    ) -> Result<(), String> {
        let result = (|| {
            let summary = prepared
                .messages
                .iter()
                .find_map(threadlane_runtime::compaction_summary_text)
                .ok_or_else(|| "context preparation produced no durable summary".to_string())?;
            let snapshots = self.compacted_context_snapshots(&prepared);
            let summary = if snapshots.is_empty() {
                summary.to_owned()
            } else {
                format!(
                    "{summary}\n\n{}",
                    render_compacted_context_index(&snapshots)
                )
            };
            let mut messages = prepared.messages.clone();
            let Some(AgentMessage::Custom { payload, .. }) = messages
                .iter_mut()
                .find(|message| threadlane_runtime::compaction_summary_text(message).is_some())
            else {
                return Err("context preparation produced no durable summary".into());
            };
            payload["summary"] = Value::String(summary.clone());
            let first_seq = self.next_seq();
            let summary_id = format!("compaction-{parent_run_id}-{first_seq}-summary");
            self.stage_open_run_compaction(parent_run_id, &summary, reason)?;

            let retained = super::durable::compaction_retained_tail(&messages);
            let mut parent_id = summary_id;
            for (index, message) in retained.into_iter().enumerate() {
                let id = format!("compaction-{parent_run_id}-{first_seq}-tail-{index}");
                let terminate = matches!(
                    &message,
                    AgentMessage::Tool {
                        terminate: true,
                        ..
                    }
                );
                self.store
                    .append_entry_gated(HarnessEntry {
                        id: id.clone(),
                        parent_id: Some(parent_id),
                        lane: "main".into(),
                        seq: first_seq + 2 + index as u64,
                        timestamp: timestamp(),
                        message,
                        surface_op: threadlane_runtime::harness::SurfaceOperation::Replace {
                            start_seq: first_seq + 2 + index as u64,
                            end_seq: (first_seq + 1 + index as u64),
                            source_event_seqs: Vec::new(),
                        },
                        terminate,
                    })
                    .map_err(|error| error.to_string())?;
                parent_id = id;
            }

            let post_tokens = estimate_request_tokens(&messages, tool_schema_json, config);
            let generation = self.compaction_generation().saturating_add(1);
            let record = HarnessRecord::ContextCompacted {
                id: format!("context-compacted-{parent_run_id}-{generation}"),
                seq: first_seq + 2 + prepared.messages.len() as u64,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: parent_run_id.into(),
                generation,
                reason,
                effective_model: TraceString::new(model)?,
                context_limit: budget.limit,
                context_limit_is_estimate: budget.limit_is_estimate,
                pre_tokens: prepared.pre_tokens,
                post_tokens,
                retained_tail_target: prepared.retained_tail_target,
                retained_tail_tokens: prepared.retained_tail_tokens,
                compacted_messages: prepared.compacted_messages,
            };
            self.store
                .append_record_gated(record)
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion_atomically()
                .map_err(|error| error.to_string())
        })();
        if result.is_err() {
            let _ = self.ensure_fresh();
        }
        result
    }

    fn compacted_context_snapshots(
        &mut self,
        prepared: &PreparedCompaction,
    ) -> Vec<threadlane_runtime::harness::ContextSnapshot> {
        let Ok(context) = self.model_context("main") else {
            return Vec::new();
        };
        let retained = super::durable::compaction_retained_tail(&prepared.messages);
        let compacted_seqs = context
            .entries
            .iter()
            .take(context.entries.len().saturating_sub(retained.len()))
            .map(|entry| entry.seq)
            .collect::<std::collections::HashSet<_>>();
        self.context_snapshots("main")
            .into_iter()
            .filter(|snapshot| {
                context
                    .entries
                    .iter()
                    .find(|entry| entry.id == snapshot.source_entry_id)
                    .is_some_and(|entry| compacted_seqs.contains(&entry.seq))
            })
            .collect()
    }

    pub(crate) fn record_manual_compaction(
        &mut self,
        run_id: &str,
        model: &str,
        config: &AgentConfig,
        pre_tokens: usize,
        retained_tail_tokens: usize,
        compacted_messages: usize,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let budget = context_budget(model, config);
        let messages = self.model_context("main")?.messages();
        let post_tokens = estimate_request_tokens(&messages, None, config);
        let generation = self.compaction_generation().saturating_add(1);
        let record = HarnessRecord::ContextCompacted {
            id: format!("context-compacted-{run_id}-{generation}"),
            seq: self.next_seq(),
            lane: "main".into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            generation,
            reason: CompactionReason::Manual,
            effective_model: TraceString::new(model)?,
            context_limit: budget.limit,
            context_limit_is_estimate: budget.limit_is_estimate,
            pre_tokens,
            post_tokens,
            retained_tail_target: budget.retained_tail_tokens,
            retained_tail_tokens,
            compacted_messages,
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }
    pub fn transcript(&self, lane: &str) -> threadlane_runtime::harness::TranscriptProjection {
        self.store.store().transcript(lane)
    }

    pub(crate) fn record_provider_trace_to_path(
        path: &Path,
        run_id: &str,
        event: ProviderTraceEvent,
    ) -> Result<(), String> {
        Self::with_path(path, |journal| journal.record_provider_trace(run_id, event))
    }

    pub(crate) fn record_provider_trace(
        &mut self,
        run_id: &str,
        event: ProviderTraceEvent,
    ) -> Result<(), String> {
        let journal = self;
        let event = match event {
            ProviderTraceEvent::AssistantReady {
                attempt,
                request_id,
                reasoning,
                message,
            } => {
                let reasoning_entry_id = if let Some(reasoning) =
                    reasoning.filter(|reasoning| !reasoning.trim().is_empty())
                {
                    let thinking = AgentMessage::Custom {
                        custom_type: "thinking".into(),
                        payload: serde_json::json!({ "text": reasoning }),
                    };
                    let existing = journal
                        .store
                        .entries()
                        .iter()
                        .rev()
                        .find(|entry| entry.message == thinking)
                        .map(|entry| entry.id.clone());
                    Some(match existing {
                        Some(id) => id,
                        None => journal.append_message(thinking)?,
                    })
                } else {
                    None
                };
                let existing = journal
                    .store
                    .entries()
                    .iter()
                    .rev()
                    .find(|entry| entry.message == message)
                    .map(|entry| entry.id.clone());
                let entry_id = match existing {
                    Some(id) => id,
                    None => journal.append_message(message)?,
                };
                let seq = harness_next_seq(journal.store.store());
                let record = HarnessRecord::ProviderResponseAttached {
                    id: format!("provider-response-{run_id}-{request_id}"),
                    seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt,
                    request_id: Some(TraceString::new(request_id)?),
                    entry_id,
                    reasoning_entry_id,
                };
                journal
                    .store
                    .append_record_gated(record)
                    .map_err(|error| error.to_string())?;
                journal
                    .store
                    .drive_to_completion()
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
            event => event,
        };
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            ProviderTraceEvent::AssistantReady { .. } => unreachable!(),
            ProviderTraceEvent::Started {
                attempt,
                request_id,
                model,
                provider,
            } => HarnessRecord::ProviderRequestStarted {
                id: format!("provider-start-{run_id}-{request_id}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                provider: TraceString::new(provider)?,
                model: TraceString::new(model)?,
                request_id: Some(TraceString::new(request_id)?),
            },
            ProviderTraceEvent::ContextManifest {
                attempt,
                request_id,
                model,
                context_limit,
                context_limit_is_estimate,
                compaction_generation,
                total_estimated_tokens,
                items,
            } => HarnessRecord::ContextManifestCaptured {
                id: format!("context-manifest-{run_id}-{request_id}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                request_id: TraceString::new(request_id)?,
                total_estimated_tokens,
                effective_model: Some(TraceString::new(model)?),
                context_limit,
                context_limit_is_estimate,
                compaction_generation,
                items,
            },
            ProviderTraceEvent::Checkpoint {
                attempt,
                request_id,
                checkpoint_index,
                text,
                reasoning,
            } => {
                let mut digest = Sha256::new();
                digest.update(text.as_bytes());
                if let Some(reasoning) = reasoning.as_deref() {
                    digest.update(reasoning.as_bytes());
                }
                HarnessRecord::StreamCheckpoint {
                    id: format!("stream-checkpoint-{run_id}-{request_id}-{checkpoint_index}"),
                    seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt: Some(attempt),
                    request_id: TraceString::new(request_id)?,
                    assistant_entry_id: None,
                    text: (!text.is_empty()).then(|| BoundedText::truncated(&text)),
                    reasoning: reasoning
                        .as_deref()
                        .filter(|reasoning| !reasoning.is_empty())
                        .map(BoundedText::truncated),
                    checkpoint_index,
                    byte_count: text.len() as u64
                        + reasoning.as_ref().map_or(0, String::len) as u64,
                    fingerprint: TraceString::new(format!("{:x}", digest.finalize()))?,
                }
            }
            ProviderTraceEvent::Finished {
                attempt,
                request_id,
                outcome,
                error,
                duration_ms,
                usage,
            } => HarnessRecord::ProviderRequestFinished {
                id: format!("provider-finish-{run_id}-{request_id}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                request_id: Some(TraceString::new(request_id)?),
                outcome,
                error,
                duration_ms: Some(duration_ms),
                usage,
            },
        };
        // Append through the journal already open above instead of reopening
        // the file; gated append re-checks freshness under the writer gate.
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn record_permission_trace_to_path(
        path: &Path,
        run_id: Option<&str>,
        event: PermissionTraceEvent,
    ) -> Result<(), String> {
        Self::with_path(path, |journal| {
            journal.record_permission_trace(run_id, event)
        })
    }

    pub(crate) fn record_permission_trace(
        &mut self,
        run_id: Option<&str>,
        event: PermissionTraceEvent,
    ) -> Result<(), String> {
        let journal = self;
        let state = Reducer::reduce(journal.store.store()).map_err(|error| error.to_string())?;
        let attempt = run_id.and_then(|_| state.lane("main").map(|lane| lane.attempts));
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            PermissionTraceEvent::Requested {
                request_id,
                capability,
                scopes,
                detail_sha256,
                source,
            } => HarnessRecord::PermissionRequested {
                id: format!("permission-request-{request_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.map(str::to_owned),
                attempt,
                request_id: TraceString::new(request_id)?,
                capability: TraceString::new(capability)?,
                scopes,
                detail_sha256: TraceString::new(detail_sha256)?,
                source,
            },
            PermissionTraceEvent::Resolved {
                request_id,
                decision,
                scope,
                source,
                remembered,
            } => HarnessRecord::PermissionResolved {
                id: format!("permission-resolved-{request_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.map(str::to_owned),
                attempt,
                request_id: TraceString::new(request_id)?,
                decision,
                scope,
                source,
                remembered,
            },
        };
        // Append through the already-open journal; see record_provider_trace_to_path.
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn record_tool_execution_to_path(
        path: &Path,
        run_id: &str,
        event: ToolExecutionTraceEvent,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.record_tool_execution(run_id, event).await
    }

    pub(crate) async fn record_tool_execution(
        &mut self,
        run_id: &str,
        event: ToolExecutionTraceEvent,
    ) -> Result<(), String> {
        let journal = self;
        if let ToolExecutionTraceEvent::Started {
            tool_call_id,
            tool_name,
            effective_arguments,
            ..
        } = &event
        {
            let has_intent = journal.store.records().iter().any(|record| {
                matches!(
                    record,
                    HarnessRecord::ToolStarted {
                        run_id: intent_run_id,
                        tool_call_id: intent_call_id,
                        ..
                    } if intent_run_id == run_id && intent_call_id == tool_call_id
                )
            });
            if !has_intent {
                let effective_args = serde_json::from_str(effective_arguments)
                    .unwrap_or_else(|_| Value::String(effective_arguments.clone()));
                journal
                    .append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
                    .await?;
            }
        }
        let state = Reducer::reduce(journal.store.store()).map_err(|error| error.to_string())?;
        let attempt = state.lane("main").map(|lane| lane.attempts);
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            ToolExecutionTraceEvent::Started {
                tool_call_id,
                tool_name,
                executor_kind,
                effective_arguments: _,
                started_at_ms,
            } => HarnessRecord::ToolExecutionObserved {
                id: format!("tool-execution-start-{run_id}-{tool_call_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                tool_call_id: TraceString::new(tool_call_id)?,
                tool_name: TraceString::new(tool_name)?,
                executor_kind: TraceString::new(executor_kind)?,
                phase: ToolExecutionPhase::Started,
                started_at_ms: Some(started_at_ms),
                duration_ms: None,
                outcome: None,
                exit_code: None,
                cancelled: false,
                is_error: None,
                terminate: None,
                output_sha256: None,
                output_bytes: None,
            },
            ToolExecutionTraceEvent::Finished {
                tool_call_id,
                tool_name,
                executor_kind,
                started_at_ms,
                duration_ms,
                is_error,
                terminate,
                output_sha256,
                output_bytes,
            } => HarnessRecord::ToolExecutionObserved {
                id: format!("tool-execution-finish-{run_id}-{tool_call_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                tool_call_id: TraceString::new(tool_call_id)?,
                tool_name: TraceString::new(tool_name)?,
                executor_kind: TraceString::new(executor_kind)?,
                phase: ToolExecutionPhase::Finished,
                started_at_ms: Some(started_at_ms),
                duration_ms: Some(duration_ms),
                outcome: Some(if is_error {
                    ToolExecutionOutcome::Failed
                } else {
                    ToolExecutionOutcome::Succeeded
                }),
                exit_code: None,
                cancelled: false,
                is_error: Some(is_error),
                terminate: Some(terminate),
                output_sha256: Some(TraceString::new(output_sha256)?),
                output_bytes: Some(output_bytes),
            },
        };
        // Append through the already-open journal; see record_provider_trace_to_path.
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn append_tool_intent_to_path(
        path: &Path,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal
            .append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
            .await
    }

    pub(crate) async fn record_tool_result_to_path(
        path: &Path,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        let path = path.to_path_buf();
        let run_id = run_id.to_owned();
        let result = result.clone();
        tokio::task::spawn_blocking(move || {
            Self::with_path(&path, |journal| {
                journal.finish_tool_result(&run_id, &result)
            })
        })
        .await
        .map_err(|error| error.to_string())?
    }

    pub(crate) fn record_tool_result(
        &mut self,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        self.finish_tool_result(run_id, result)
    }

    pub(crate) fn start_subagent_lane(
        &mut self,
        lane_hint: &str,
        task: &str,
        source_leaf_id: Option<&str>,
    ) -> Result<StartedSubagentLane, SubagentStartError> {
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
            self.ensure_fresh().map_err(|error| SubagentStartError {
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
            let mut identity = SubagentLaneIdentity {
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
                    identity.source_leaf_id = None;
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
        let prompt_entry_id = format!("entry-{}-user", identity.run_id);
        let effective_parent_id = source_leaf_id
            .filter(|id| self.store.entries().iter().any(|e| e.id == *id))
            .map(str::to_owned);
        self.store
            .append_entry_gated(HarnessEntry {
                id: prompt_entry_id,
                parent_id: effective_parent_id,
                lane: identity.lane_name.clone(),
                seq: harness_next_seq(self.store.store()),
                timestamp: timestamp(),
                message: prompt_message,
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
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
        let state = Reducer::reduce(self.store.store()).map_err(|error| SubagentStartError {
            identity: Some(identity.clone()),
            error: error.to_string(),
        })?;
        let parent_run_id = state
            .lane("main")
            .and_then(|lane| lane.open_operation.clone());
        let parent_attempt = state.lane("main").map(|lane| lane.attempts);
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::SubagentLifecycle {
                id: format!("subagent-started-{}-{seq}", identity.run_id),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: parent_run_id,
                attempt: parent_attempt,
                child_run_id: TraceString::new(identity.run_id.clone()).map_err(|error| {
                    SubagentStartError {
                        identity: Some(identity.clone()),
                        error,
                    }
                })?,
                parent_tool_call_id: None,
                task_index: None,
                agent_id: TraceString::new(lane_hint).map_err(|error| SubagentStartError {
                    identity: Some(identity.clone()),
                    error,
                })?,
                subagent_lane: TraceString::new(identity.lane_name.clone()).map_err(|error| {
                    SubagentStartError {
                        identity: Some(identity.clone()),
                        error,
                    }
                })?,
                phase: SubagentLifecyclePhase::Started,
                result_entry_id: None,
                error: None,
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
        let identity = SubagentLaneIdentity {
            started_seq: self
                .store
                .records()
                .iter()
                .find_map(|record| match record {
                    HarnessRecord::OperationStarted { id, seq, .. } if id == &identity.run_id => {
                        Some(*seq)
                    }
                    _ => None,
                })
                .unwrap_or(0),
            ..identity
        };
        let accepted =
            self.accepted_subagent_run(&identity)
                .map_err(|error| SubagentStartError {
                    identity: Some(identity.clone()),
                    error,
                })?;
        Ok(StartedSubagentLane { identity, accepted })
    }

    pub(crate) fn accepted_subagent_run(
        &self,
        identity: &SubagentLaneIdentity,
    ) -> Result<AcceptedRun, String> {
        let accepted = AcceptedRun {
            session_id: self.store.session_id().to_owned(),
            run_id: identity.run_id.clone(),
            lane: identity.lane_name.clone(),
            prompt_entry_id: format!("entry-{}-user", identity.run_id),
            assistant_entry_id: format!("entry-{}-assistant-1", identity.run_id),
            accepted_through_seq: self
                .store
                .entries()
                .iter()
                .map(|entry| entry.seq)
                .chain(self.store.records().iter().map(HarnessRecord::seq))
                .max()
                .unwrap_or(0),
        };
        self.store
            .validate_accepted_run(&accepted)
            .map_err(|error| error.to_string())?;
        Ok(accepted)
    }

    pub(crate) fn append_subagent_context(
        &mut self,
        lane: &str,
        run_id: &str,
        message: String,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let prompt_entry_id = format!("entry-{run_id}-user");
        if !self
            .store
            .entries()
            .iter()
            .any(|entry| entry.id == prompt_entry_id && entry.lane == lane)
        {
            return Err(format!("Missing accepted subagent task for lane {lane}"));
        }
        self.store
            .append_entry_gated(HarnessEntry {
                id: format!("entry-{run_id}-context-1"),
                parent_id: Some(prompt_entry_id),
                lane: lane.into(),
                seq: harness_next_seq(self.store.store()),
                timestamp: timestamp(),
                message: AgentMessage::user(message, Vec::new()),
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn finish_subagent_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let is_open = Reducer::reduce(self.store.store()).ok().map(|state| {
            state
                .lanes
                .iter()
                .any(|l| l.open_operation.as_deref() == Some(run_id))
        }) == Some(true);
        if !is_open {
            return Ok(());
        }

        if outcome == OperationOutcome::Aborted {
            let mut any_provisioned = false;
            if let Ok(state) = Reducer::reduce(self.store.store()) {
                if let Some(l) = state
                    .lanes
                    .iter()
                    .find(|l| l.open_operation.as_deref() == Some(run_id))
                {
                    for tool in &l.tools {
                        if !tool.completed
                            && tool.run_id == run_id
                            && !self
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
            if any_provisioned {
                let _ = self.refresh();
            }
            let _ = self.store.request_abort(run_id);
            let _ = self.store.drive_to_completion();
            let _ = self.refresh();
            if self.store.reconcile_abort_run(run_id).is_ok() {
                let _ = self.store.drive_to_completion();
                return Ok(());
            }
        }

        self.store
            .finish_operation(run_id, outcome.clone(), error.clone())
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;

        let phase = match outcome {
            OperationOutcome::Completed => SubagentLifecyclePhase::Completed,
            OperationOutcome::Failed => SubagentLifecyclePhase::Failed,
            OperationOutcome::Aborted | OperationOutcome::Declined => {
                SubagentLifecyclePhase::Cancelled
            }
        };
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::SubagentLifecycle {
                id: format!("subagent-finished-{run_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: None,
                attempt: None,
                child_run_id: TraceString::new(run_id.to_owned())?,
                parent_tool_call_id: None,
                task_index: None,
                agent_id: TraceString::new(lane.to_owned())?,
                subagent_lane: TraceString::new(lane.to_owned())?,
                phase,
                result_entry_id: None,
                error: error.map(TraceString::new).transpose()?,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn checkpoint(
        &mut self,
        lane: &str,
        run_id: &str,
        messages: &[AgentMessage],
    ) -> Result<(), String> {
        if messages.is_empty() {
            return Ok(());
        }
        self.ensure_fresh()?;
        for message in messages {
            self.append_message_to_lane(lane, run_id, message.clone())?;
        }
        Ok(())
    }

    // ── Run lifecycle ─────────────────────────────────────────────────

    /// Start a foreground operation and accept the user prompt.
    ///
    /// Returns `Ok(AcceptedRun)` after `accept_prompt` is driven to completion
    /// (committed to the JSONL store).
    pub(crate) fn begin_run(
        &mut self,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<AcceptedRun, String> {
        self.ensure_fresh()?;
        self.store
            .accept_prompt_and_drive_on_lane(&self.main_lane_name, run_id, prompt)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn begin_run_text(&mut self, prompt: &str) -> Result<AcceptedRun, String> {
        let run_id = format!(
            "run-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        self.begin_run(&run_id, AgentMessage::user(prompt.to_string(), Vec::new()))
    }

    pub(crate) fn enqueue_unbound_with_images(
        &mut self,
        queue: QueueKind,
        content: String,
        images: Vec<ImageAttachment>,
    ) -> Result<String, String> {
        self.ensure_fresh()?;
        let id = format!(
            "entry-queue-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let target = ProvisionedEntry::new(&id, None, AgentMessage::user(content, images));
        self.store
            .enqueue_unbound(queue, target)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    pub(crate) fn enqueue_unbound_on_lane_with_priority(
        &mut self,
        lane: &str,
        queue: QueueKind,
        target: ProvisionedEntry,
        priority: Option<threadlane_runtime::SteerPriority>,
    ) -> Result<String, String> {
        self.ensure_fresh()?;
        let id = target.id.clone();
        if let Some(priority) = priority {
            let underlying = self.store.store();
            let queue_seq = underlying
                .entries()
                .iter()
                .map(|e| e.seq)
                .chain(underlying.records().iter().map(|r| r.seq()))
                .max()
                .unwrap_or(0)
                + 1;
            let record_id = format!(
                "queue-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            );
            self.store
                .append_record_gated(threadlane_runtime::harness::Record::QueueEnqueued {
                    id: record_id,
                    seq: queue_seq,
                    lane: lane.to_string(),
                    timestamp: queue_seq,
                    run_id: None,
                    queue,
                    priority: Some(priority),
                    target,
                })
                .map_err(|error| error.to_string())?;
        } else {
            self.store
                .enqueue_unbound_on_lane(lane, queue, target)
                .map_err(|error| error.to_string())?;
        }
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    pub(crate) fn consume_first_unbound_queue(&mut self, queue: QueueKind) -> Result<(), String> {
        self.ensure_fresh()?;
        let state = Reducer::reduce(self.store.store())
            .map_err(|error| format!("reduce failed: {error:?}"))?;
        let entry_id = state
            .lane(&self.main_lane_name)
            .and_then(|lane| {
                lane.queued
                    .iter()
                    .find(|q| q.run_id.is_none() && q.queue == queue)
            })
            .map(|queued| queued.target.id.clone());
        let Some(entry_id) = entry_id else {
            return Ok(());
        };
        self.store
            .consume_unbound(&entry_id)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(crate) fn consume_unbound_queue_entry(
        &mut self,
        queue: QueueKind,
        entry_id: &str,
    ) -> Result<Option<AgentMessage>, String> {
        self.ensure_fresh()?;
        let state = Reducer::reduce(self.store.store())
            .map_err(|error| format!("reduce failed: {error:?}"))?;
        let lane = state
            .lane(&self.main_lane_name)
            .ok_or_else(|| format!("unknown lane: {}", self.main_lane_name))?;
        let Some(queued) = lane
            .queued
            .iter()
            .find(|q| q.run_id.is_none() && q.queue == queue && q.target.id == entry_id)
            .cloned()
        else {
            return Ok(None);
        };
        let message = queued.target.message.clone();
        self.store
            .consume_unbound(&queued.target.id)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(Some(message))
    }

    pub(crate) fn cancel_queued_unbound(&mut self, entry_id: &str) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .cancel_unbound(entry_id)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Validate an accepted run token against the session journal and reduced state.
    pub(crate) fn validate_accepted_run(&self, accepted: &AcceptedRun) -> Result<(), String> {
        self.store
            .validate_accepted_run(accepted)
            .map_err(|error| error.to_string())
    }

    /// Append a tool intent.
    pub(crate) async fn append_tool_intent(
        &mut self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        if self.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ToolStarted {
                run_id: record_run_id,
                tool_call_id: record_call_id,
                ..
            } if record_run_id == run_id && record_call_id == tool_call_id)
        }) {
            return Ok(());
        }
        self.run_before_tool_hook(run_id, tool_call_id, tool_name)
            .await?;
        self.append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
            .await
    }

    pub(crate) async fn run_before_tool_hook(
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
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            tool_arguments: None,
            tool_result_content: None,
            tool_result_is_error: None,
        };
        self.store
            .hooks()
            .run_before_tool(&context)
            .await
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

    /// Start a foreground operation with an optional prompt.
    pub(crate) fn start(
        &mut self,
        run_id: &str,
        prompt: Option<AgentMessage>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .start_operation(run_id, None, OperationIntent::Run)
            .map_err(|error| error.to_string())?;
        if let Some(msg) = prompt {
            self.store
                .accept_prompt(run_id, msg)
                .map_err(|error| error.to_string())?;
        }
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish an operation with the given outcome and optional error.
    pub(crate) fn finish(
        &mut self,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.finish_run(run_id, outcome, error)
    }

    /// Finish an operation with the given outcome and optional error.
    pub(crate) fn finish_run(
        &mut self,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.store
            .finish_operation(run_id, outcome, error)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Generate a unique run identifier scoped to this session.
    pub(crate) fn unique_run_id(&mut self, prefix: &str) -> Result<String, String> {
        self.ensure_fresh()?;
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

    // ── Cancellation ──────────────────────────────────────────────────

    /// Request abort for all open lanes and return the main lane's run id,
    /// if any.
    pub(crate) fn request_abort(&mut self) -> Result<Option<String>, String> {
        self.cancellation.store(true, Ordering::SeqCst);
        self.ensure_fresh()?;
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

    pub(crate) fn observe_abort_signal(
        &mut self,
        run_id: &str,
        acknowledged: bool,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let attempt = state.lane("main").map(|lane| lane.attempts);
        let unfinished_requests = self
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestStarted {
                    run_id: provider_run_id,
                    attempt,
                    request_id: Some(request_id),
                    ..
                } if provider_run_id == run_id
                    && !self.store.records().iter().any(|candidate| {
                        matches!(
                            candidate,
                            HarnessRecord::ProviderRequestFinished {
                                run_id: finished_run_id,
                                request_id: Some(finished_request_id),
                                ..
                            } if finished_run_id == run_id && finished_request_id == request_id
                        )
                    }) =>
                {
                    Some((*attempt, request_id.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (provider_attempt, request_id) in unfinished_requests {
            let seq = harness_next_seq(self.store.store());
            self.store
                .append_record_gated(HarnessRecord::ProviderRequestFinished {
                    id: format!("provider-finish-{run_id}-{}", request_id.as_str()),
                    seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt: provider_attempt,
                    request_id: Some(request_id),
                    outcome: ProviderOutcome::Aborted,
                    error: Some(ProviderErrorSummary {
                        category: ErrorCategory::Cancelled,
                        code: TraceString::new("runtime_abort").ok(),
                        retryable: false,
                    }),
                    duration_ms: None,
                    usage: None,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::AbortObserved {
                id: format!("abort-observed-{run_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                observation: AbortObservation::SignalSent,
                initiator: AbortInitiator::User,
                target: AbortTarget::ActiveRun,
                acknowledged,
                detail: None,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Reconcile an aborted operation: insert abort entry, record, and
    /// finish with `Aborted` outcome.  Returns `true` if recovery produced
    /// a terminal state.
    pub(crate) fn recover_abort(&mut self) -> Result<bool, String> {
        self.ensure_fresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let Some(lane) = state.lane("main") else {
            return Ok(false);
        };
        let Some(run_id) = lane.open_operation.clone() else {
            return Ok(false);
        };
        if !lane.abort_requested {
            self.store
                .request_abort(&run_id)
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
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
                    AgentMessage::Assistant {
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
                .append_entry_gated(HarnessEntry {
                    id: entry_id.clone(),
                    parent_id: lane.leaf_id.clone(),
                    lane: "main".into(),
                    seq,
                    timestamp: timestamp(),
                    message: AgentMessage::Assistant {
                        content: Some("Run aborted before completion.".into()),
                        tool_calls: None,
                        stop_reason: Some("aborted".into()),
                        deferred_handle: None,
                    },
                    surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                    terminate: false,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        self.finish_run(
            &run_id,
            OperationOutcome::Aborted,
            Some("Generation cancelled".into()),
        )?;
        Ok(true)
    }

    // ── Assistant attempt & messages ──────────────────────────────────

    /// Append a user/assistant/tool message as a harness entry on the main
    /// lane.
    pub(crate) fn append_message(&mut self, message: AgentMessage) -> Result<String, String> {
        self.append_message_inner(message, true, false)
    }

    /// Append a message discovered while reconciling the provider transcript.
    ///
    /// Transcript synchronization already determines whether this is a new
    /// occurrence.  It must not apply the legacy last-entry content check,
    /// because two consecutive provider messages can legitimately have the
    /// same serialized value.
    fn append_synced_message(&mut self, message: AgentMessage) -> Result<String, String> {
        self.append_message_inner(message, false, false)
    }

    /// Restores a retained compaction-tail occurrence to model context.
    ///
    /// A replacement with an empty range changes no prior context entries. Its
    /// non-append surface metadata identifies this as context restoration rather
    /// than a second human-visible transcript occurrence.
    pub(crate) fn append_message_occurrence(
        &mut self,
        message: AgentMessage,
    ) -> Result<String, String> {
        self.append_message_inner(message, false, true)
    }

    fn append_message_inner(
        &mut self,
        message: AgentMessage,
        deduplicate_last_entry: bool,
        context_restoration: bool,
    ) -> Result<String, String> {
        self.ensure_fresh()?;
        if deduplicate_last_entry {
            if let Some(entry) = self.store.entries().last() {
                if entry.message == message {
                    return Ok(entry.id.clone());
                }
            }
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
        // Tool completions are recorded both by the execution lifecycle and
        // by the model-visible transcript.  They may be separated by other
        // journal records, so checking only the last entry is insufficient.
        if self
            .store
            .entries()
            .iter()
            .any(|entry| entry.id == id && entry.message == message)
        {
            return Ok(id);
        }
        self.store
            .append_entry_gated(HarnessEntry {
                id: id.clone(),
                parent_id,
                lane: "main".into(),
                seq,
                timestamp: timestamp(),
                message,
                surface_op: if context_restoration {
                    threadlane_runtime::harness::SurfaceOperation::Replace {
                        start_seq: seq,
                        end_seq: seq.saturating_sub(1),
                        source_event_seqs: Vec::new(),
                    }
                } else {
                    threadlane_runtime::harness::SurfaceOperation::Append
                },
                terminate,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    /// Append a message to a named lane (used for subagent results).
    pub(crate) fn append_message_to_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        message: AgentMessage,
    ) -> Result<String, String> {
        self.ensure_fresh()?;
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
                        lane: record_lane,
                        run_id: record_run,
                        tool_call_id: id,
                        assistant_entry_id,
                        ..
                    } if record_lane == lane && record_run == run_id && id == tool_call_id => {
                        Some(assistant_entry_id.clone())
                    }
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
        let seq = self.next_seq();
        let terminate = matches!(
            &message,
            AgentMessage::Tool {
                terminate: true,
                ..
            }
        );
        let entry = HarnessEntry {
            id: id.clone(),
            seq,
            lane: lane.into(),
            parent_id,
            timestamp: timestamp(),
            message,
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
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

    /// Prepare an assistant attempt record for the given run.  Returns
    /// the result entry id that the assistant message should carry.
    pub(crate) fn prepare_assistant_attempt(&mut self, run_id: &str) -> Result<String, String> {
        self.ensure_fresh()?;
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

    /// Record a completed assistant attempt after the assistant message
    /// has been appended.
    pub(crate) fn record_assistant_attempt(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
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
                entry.seq > start_seq && matches!(&entry.message, AgentMessage::Assistant { .. })
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

    // ── Tools ─────────────────────────────────────────────────────────

    /// Record a tool intent (after hooks have run).
    pub(crate) async fn append_tool_intent_after_hook(
        &mut self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        if self.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ToolStarted {
                run_id: record_run_id,
                tool_call_id: record_call_id,
                ..
            } if record_run_id == run_id && record_call_id == tool_call_id)
        }) {
            return Ok(());
        }
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
                    replay: match threadlane_runtime::classify_tool_replay_safety(tool_name) {
                        threadlane_runtime::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                        threadlane_runtime::ToolReplaySafety::Never => {
                            HarnessToolReplaySafety::Never
                        }
                    },
                }],
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Record tool-started on a specific lane (subagent support).
    pub(crate) fn tool_started_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        if self.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ToolStarted {
                run_id: record_run_id,
                tool_call_id: record_call_id,
                ..
            } if record_run_id == run_id && record_call_id == tool_call_id)
        }) {
            return Ok(());
        }
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
            replay: match threadlane_runtime::classify_tool_replay_safety(tool_name) {
                threadlane_runtime::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                threadlane_runtime::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
            },
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish a tool message: record ToolFinished and drive effects.
    pub(crate) fn finish_tool_message(
        &mut self,
        run_id: &str,
        message: &AgentMessage,
    ) -> Result<(), String> {
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
        self.ensure_fresh()?;
        self.store
            .finish_existing_tool(
                run_id,
                threadlane_runtime::harness::ToolResult {
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

    /// Finish a freshly executed tool result: record the tool result Entry, ToolFinished, and drive effects.
    pub(crate) fn finish_tool_result(
        &mut self,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .finish_tool(
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

    /// Finish a replayed tool result.
    pub(crate) fn finish_replayed_tool(
        &mut self,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
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

    /// Record tool completions with termination flags.
    pub(crate) fn record_completed_tools_with_termination(
        &mut self,
        run_id: &str,
        termination: &HashMap<String, bool>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
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
            .filter(|entry| {
                matches!(&entry.message,
                AgentMessage::Assistant {
                    tool_calls: Some(tool_calls),
                    ..
                } if !tool_calls.is_empty())
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
            .filter(|entry| entry.seq > assistant.seq)
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
                .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));
            let replay = match threadlane_runtime::classify_tool_replay_safety(name) {
                threadlane_runtime::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                threadlane_runtime::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
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
            let terminate = termination.get(&call.id).copied().unwrap_or(false);
            self.store
                .finish_existing_tool(
                    run_id,
                    threadlane_runtime::harness::ToolResult {
                        call_id: call.id.clone(),
                        name: name.clone(),
                        content: persisted_result.0,
                        is_error: persisted_result.1,
                        terminate,
                    },
                )
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    // ── Usage ─────────────────────────────────────────────────────────

    /// Record provider token usage for a run.
    pub(crate) fn record_provider_usage(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .record_provider_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Record discarded (non-terminal) token usage.
    pub(crate) fn record_discarded_usage(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .record_discarded_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Retry ─────────────────────────────────────────────────────────

    /// Schedule a retry for a failed run.
    pub(crate) fn schedule_retry(&mut self, run_id: &str, reason: &str) -> Result<u32, String> {
        self.ensure_fresh()?;
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

    /// Begin a previously scheduled retry attempt.
    pub(crate) fn begin_retry(&mut self, run_id: &str) -> Result<u32, String> {
        self.ensure_fresh()?;
        let attempt = self
            .store
            .begin_retry(run_id)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(attempt)
    }

    // ── Deferred ──────────────────────────────────────────────────────

    /// Redeem a deferred operation and optionally finish the run.
    pub(crate) fn redeem_deferred(
        &mut self,
        run_id: &str,
        resolution: DeferredResolution,
    ) -> Result<bool, String> {
        self.ensure_fresh()?;
        let terminal = self
            .store
            .redeem_deferred(run_id, resolution)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        if terminal {
            self.finish_run(run_id, OperationOutcome::Completed, None)?;
        }
        Ok(terminal)
    }

    // ── Compaction ────────────────────────────────────────────────────

    /// Accept a compaction summary.
    pub(crate) fn accept_compaction(&mut self, run_id: &str, summary: &str) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .accept_compaction(run_id, summary)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Facts ─────────────────────────────────────────────────────────

    /// Set a session-level fact.
    pub(crate) fn set_fact(&mut self, lane: &str, key: &str, value: String) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .set_fact(lane, key, value, None)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Replay & navigation ───────────────────────────────────────────

    /// Append a replayed tool entry to the store.
    pub(crate) fn append_replayed_tool_entry(
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
        let parent_id = if spec.index == 0 {
            assistant_entry_id.to_string()
        } else {
            state
                .lanes
                .iter()
                .flat_map(|lane| lane.tools.iter())
                .find(|tool| {
                    tool.run_id == run_id
                        && tool.assistant_entry_id == assistant_entry_id
                        && tool.tool_index + 1 == spec.index
                })
                .filter(|tool| {
                    self.store
                        .entries()
                        .iter()
                        .any(|entry| entry.id == tool.result_entry_id)
                })
                .map(|tool| tool.result_entry_id.clone())
                .unwrap_or_else(|| assistant_entry_id.to_string())
        };
        let seq = self.next_seq();
        self.store
            .append_entry_gated(HarnessEntry {
                id: spec.result_entry_id.clone(),
                parent_id: Some(parent_id),
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
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: result.terminates(),
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Claim safe tool replays for recovery.
    pub(crate) fn claim_safe_replays(
        &mut self,
        tools: &[HarnessRecord],
    ) -> Result<Vec<HarnessRecord>, String> {
        let records = self.store.records().to_vec();
        let entries = self.store.entries().to_vec();
        let mut claimed = Vec::new();
        for tool in tools {
            let HarnessRecord::ToolStarted {
                lane,
                run_id,
                assistant_entry_id,
                tool_index,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                replay: HarnessToolReplaySafety::Safe,
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

    /// Materialize a session branch path as harness entries.
    pub(crate) fn navigate_branch(
        &mut self,
        branch_ids: &[String],
    ) -> Result<Option<String>, String> {
        self.ensure_fresh()?;
        let mut harness_target_id = None;
        for legacy_id in branch_ids {
            let entry = self
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == *legacy_id)
                .cloned()
                .ok_or_else(|| format!("Entry ID not found in session: {legacy_id}"))?;
            if matches!(entry.message, AgentMessage::System { .. }) {
                continue;
            }
            if *legacy_id == branch_ids[branch_ids.len() - 1] {
                harness_target_id = Some(entry.id);
            }
        }
        Ok(harness_target_id)
    }

    // ── Observation ───────────────────────────────────────────────────

    /// Take a point-in-time snapshot of the session.
    pub(crate) fn snapshot(&mut self) -> Result<Snapshot, String> {
        self.ensure_fresh()?;
        self.store.snapshot().map_err(|error| error.to_string())
    }

    /// Subscribe to session-scoped events.
    pub(crate) fn watch(&mut self) -> Result<HarnessWatch, String> {
        self.ensure_fresh()?;
        let subscription = self
            .store
            .watch_session()
            .map_err(|error| error.to_string())?;
        Ok(HarnessWatch {
            hub: self.events.clone(),
            subscription,
        })
    }

    /// Drive all pending effects to completion.
    pub(crate) fn drive_to_completion(&mut self) -> Result<(), String> {
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Internal ──────────────────────────────────────────────────────

    /// Re-read the store from disk to pick up external writes.
    pub(crate) fn refresh(&mut self) -> Result<(), String> {
        self.ensure_fresh()
    }

    fn next_seq(&self) -> u64 {
        self.store.store().next_sequence()
    }

    /// Legacy test/repair helper for journal-free or recovery-only callers.
    ///
    /// Production provider execution must commit typed transitions through the
    /// run-scoped recorders before the next request; it must not reconcile a
    /// complete mutable provider transcript after the fact.
    #[cfg(test)]
    pub(crate) fn sync_messages(&mut self, messages: &[AgentMessage]) -> Result<(), String> {
        self.ensure_fresh()?;
        // The provider gives us the complete conversation, not stable entry
        // IDs.  Track occurrences rather than using a set: two turns can
        // legitimately produce byte-for-byte identical assistant messages,
        // including an empty assistant result.
        let mut existing: HashMap<String, usize> = HashMap::new();
        for entry in self
            .store
            .model_context("main")
            .map_err(|error| error.to_string())?
            .entries
        {
            *existing.entry(format!("{:?}", entry.message)).or_default() += 1;
        }

        for msg in messages {
            if matches!(msg, AgentMessage::System { .. }) {
                continue;
            }
            // Initial prompts are already present through begin_run, while
            // queued/steered/generated user messages may exist only in the
            // provider transcript. Occurrence matching handles both cases.
            let key = format!("{:?}", msg);
            if let Some(count) = existing.get_mut(&key) {
                if *count > 0 {
                    *count -= 1;
                    continue;
                }
            }
            if let AgentMessage::Tool { tool_call_id, .. } = msg {
                self.ensure_fresh()?;
                let unfinished_tool = Reducer::reduce(self.store.store()).ok().and_then(|state| {
                    let lane = state.lane("main")?;
                    let run_id = lane.open_operation.as_deref()?;
                    lane.tools.iter().find_map(|tool| {
                        (tool.run_id == run_id
                            && tool.tool_call_id == *tool_call_id
                            && !tool.completed)
                            .then(|| (run_id.to_owned(), tool.result_entry_id.clone()))
                    })
                });
                if let Some((run_id, result_entry_id)) = unfinished_tool {
                    // ToolStarted may be durable while its result entry is
                    // not, if the process was interrupted between those
                    // writes. Recreate the entry before closing the intent.
                    if !self
                        .store
                        .entries()
                        .iter()
                        .any(|entry| entry.id == result_entry_id)
                    {
                        self.append_synced_message(msg.clone())?;
                    }
                    self.finish_tool_message(&run_id, msg)?;
                    continue;
                }
            }
            self.append_synced_message(msg.clone())?;
        }
        Ok(())
    }
    pub(crate) fn assert_model_visible(&mut self, messages: &[AgentMessage]) -> Result<(), String> {
        self.ensure_fresh()?;
        let logged = self
            .store
            .model_context("main")
            .map_err(|error| error.to_string())?
            .messages();
        let expected = messages
            .iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .cloned()
            .collect::<Vec<_>>();
        if logged == expected {
            return Ok(());
        }
        let mismatch = logged
            .iter()
            .zip(expected.iter())
            .position(|(logged, expected)| logged != expected)
            .unwrap_or_else(|| logged.len().min(expected.len()));
        Err(format!(
            "model-visible history diverges at index {mismatch}: durable_count={}, provider_count={}",
            logged.len(),
            expected.len()
        ))
    }

    pub(crate) fn commit_assistant_message(
        &mut self,
        lane: &str,
        run_id: &str,
        content: Option<String>,
        stop_reason: Option<String>,
    ) -> Result<String, String> {
        self.append_message_to_lane(
            lane,
            run_id,
            AgentMessage::Assistant {
                content,
                tool_calls: None,
                stop_reason,
                deferred_handle: None,
            },
        )
    }

    pub(crate) fn commit_thinking(
        &mut self,
        lane: &str,
        run_id: &str,
        reasoning: String,
    ) -> Result<String, String> {
        self.append_message_to_lane(
            lane,
            run_id,
            AgentMessage::Custom {
                custom_type: "thinking".to_string(),
                payload: Value::String(reasoning),
            },
        )
    }

    pub(crate) fn commit_tool_calls(
        &mut self,
        lane: &str,
        run_id: &str,
        tool_calls: Vec<threadlane_provider::openai::ToolCall>,
    ) -> Result<String, String> {
        self.append_message_to_lane(
            lane,
            run_id,
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(tool_calls),
                stop_reason: None,
                deferred_handle: None,
            },
        )
    }

    pub(crate) fn commit_tool_results(
        &mut self,
        lane: &str,
        run_id: &str,
        results: &[AgentToolResult],
    ) -> Result<Vec<String>, String> {
        let mut committed = Vec::new();
        for result in results {
            let msg = AgentMessage::Tool {
                tool_call_id: result.tool_call_id.clone(),
                name: result.name.clone(),
                content: result.content.clone(),
                is_error: result.is_error,
                terminate: result.terminates(),
            };
            let entry_id = self.append_message_to_lane(lane, run_id, msg)?;
            let _ = self.finish_tool_result(run_id, result);
            committed.push(entry_id);
        }
        Ok(committed)
    }

    pub(crate) fn commit_follow_up(
        &mut self,
        lane: &str,
        run_id: &str,
        message: AgentMessage,
    ) -> Result<String, String> {
        self.append_message_to_lane(lane, run_id, message)
    }

    pub(crate) fn commit_provider_failure(
        &mut self,
        _lane: &str,
        run_id: &str,
        error: String,
    ) -> Result<(), String> {
        self.finish_run(run_id, OperationOutcome::Failed, Some(error))
    }

    pub(crate) fn plan_recovery(
        &mut self,
        lane: &str,
    ) -> Result<threadlane_runtime::harness::RecoveryPlan, String> {
        self.ensure_fresh()?;
        let agent = threadlane_runtime::harness::SessionAgent::new(AgentHarness::new(
            self.store.store().clone(),
        ));
        let lane_handle = threadlane_runtime::harness::LaneHandle::new(lane.to_string())
            .map_err(|error| error.to_string())?;
        agent
            .plan_recovery(&lane_handle)
            .map_err(|error| error.to_string())
    }

    /// Run hooks of the given kind for the main lane.
    pub(crate) async fn run_hooks(&self, kind: HookKind, context: &HookContext) {
        for failure in self.store.hooks().run(kind, context).await {
            eprintln!(
                "hook {} ({:?}) failed: {}",
                failure.id, kind, failure.message
            );
        }
    }
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
mod tests {
    use super::*;
    fn temp_session() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        (dir, path)
    }

    fn open_long_run(path: &Path) -> CodingSessionHarness {
        let mut harness = CodingSessionHarness::open(path).unwrap();
        harness
            .begin_run("run-compact", AgentMessage::user("start", vec![]))
            .unwrap();
        for index in 0..28 {
            harness
                .append_message(AgentMessage::user(
                    format!("history-{index}-{}", "x".repeat(16_000)),
                    vec![],
                ))
                .unwrap();
        }
        harness
    }

    #[test]
    fn recover_abort_terminates_suspended_foreground_run_without_prior_cancel() {
        let (_dir, path) = temp_session();
        {
            let mut harness = CodingSessionHarness::open(&path).unwrap();
            harness
                .begin_run("interrupted-run", AgentMessage::user("first", vec![]))
                .unwrap();
        }

        let mut reopened = CodingSessionHarness::open(&path).unwrap();
        assert!(reopened.recover_abort().unwrap());

        let state = Reducer::reduce(&reopened.store).unwrap();
        let main = state.lane("main").unwrap();
        assert!(main.open_operation.is_none());
        assert!(reopened.store.records().iter().any(|record| {
            matches!(
                record,
                HarnessRecord::OperationFinished {
                    run_id,
                    outcome: OperationOutcome::Aborted,
                    ..
                } if run_id == "interrupted-run"
            )
        }));

        reopened
            .begin_run("next-run", AgentMessage::user("continue", vec![]))
            .unwrap();
    }

    fn boundary_request(overflow_recovery: bool) -> ProviderBoundaryRequest {
        ProviderBoundaryRequest {
            attempt: 1,
            model: "unknown/test-model".into(),
            messages: Vec::new(),
            tool_schema_json: None,
            overflow_recovery,
        }
    }

    #[test]
    fn provider_identity_survives_reopen_of_same_open_run() {
        let (_dir, path) = temp_session();
        let accepted = {
            let mut harness = CodingSessionHarness::open(&path).unwrap();
            let accepted = harness
                .begin_run("restart-run", AgentMessage::user("hello", vec![]))
                .unwrap();
            let first = harness
                .prepare_provider_boundary(
                    "restart-run",
                    boundary_request(false),
                    &AgentConfig::default(),
                )
                .unwrap();
            let attempt = first.provider_attempt.unwrap();
            let request_id = first.provider_request_id.unwrap();
            harness
                .record_provider_trace(
                    "restart-run",
                    ProviderTraceEvent::Started {
                        attempt,
                        request_id,
                        model: "unknown/test-model".into(),
                        provider: "fake".into(),
                    },
                )
                .unwrap();
            accepted
        };

        let mut resumed = CodingSessionHarness::open(&path).unwrap();
        resumed.validate_accepted_run(&accepted).unwrap();
        let second = resumed
            .prepare_provider_boundary(
                "restart-run",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .unwrap();
        assert_eq!(second.provider_attempt, Some(2));
        let second_request_id = second.provider_request_id.clone().unwrap();
        resumed
            .record_provider_trace(
                "restart-run",
                ProviderTraceEvent::Started {
                    attempt: second.provider_attempt.unwrap(),
                    request_id: second_request_id,
                    model: "unknown/test-model".into(),
                    provider: "fake".into(),
                },
            )
            .unwrap();

        let starts: Vec<_> = resumed
            .store
            .store()
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestStarted {
                    id,
                    attempt,
                    request_id: Some(request_id),
                    ..
                } => Some((id, *attempt, request_id.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!((starts[0].1, starts[1].1), (1, 2));
        assert_ne!(starts[0].0, starts[1].0);
        assert_ne!(starts[0].2, starts[1].2);
        assert!(Reducer::reduce(resumed.store.store()).is_ok());
    }
    #[test]
    fn adaptive_compaction_commits_before_next_provider_attempt() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .unwrap();
        harness
            .record_provider_trace(
                "run-compact",
                ProviderTraceEvent::Started {
                    attempt: 1,
                    request_id: "request-after-compaction".into(),
                    model: "unknown/test-model".into(),
                    provider: "fake".into(),
                },
            )
            .unwrap();
        let compacted_seq = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    seq,
                    reason: CompactionReason::AdaptiveBudget,
                    ..
                } => Some(*seq),
                _ => None,
            })
            .expect("adaptive compaction");
        let start_seq = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ProviderRequestStarted { seq, .. } if *seq > compacted_seq => {
                    Some(*seq)
                }
                _ => None,
            })
            .expect("provider start after compaction");
        assert!(compacted_seq < start_seq);
        assert_eq!(
            Reducer::reduce(&harness.store)
                .unwrap()
                .lane("main")
                .unwrap()
                .open_operation
                .as_deref(),
            Some("run-compact"),
            "provider-boundary compaction must preserve the foreground run"
        );
    }

    #[test]
    fn reload_uses_checkpoint_tail_but_transcript_keeps_original_entries() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .unwrap();
        drop(harness);
        let reloaded = CodingSessionHarness::open(&path).unwrap();
        let context = reloaded.model_context("main").unwrap();
        assert!(context.checkpoint.is_some());
        assert!(reloaded.transcript("main").entries.len() > context.entries.len());
    }

    #[cfg(unix)]
    #[test]
    fn compaction_persistence_failure_appends_no_checkpoint_prefix() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        let original = fs::metadata(&path).unwrap().permissions();
        let canonical = fs::read(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        let result = harness.prepare_provider_boundary(
            "run-compact",
            boundary_request(false),
            &AgentConfig::default(),
        );
        fs::set_permissions(&path, original).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), canonical);
        assert!(!harness
            .store
            .records()
            .iter()
            .any(|record| matches!(record, HarnessRecord::ProviderRequestStarted { .. })));
    }

    #[test]
    fn ineffective_compaction_retries_once() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run(
                "run-compact",
                AgentMessage::user("x".repeat(50_000), vec![]),
            )
            .unwrap();
        harness
            .append_message(AgentMessage::user("y".repeat(50_000), vec![]))
            .unwrap();
        // The oversized accepted tail survives the normal checkpoint, so the
        // strict pass is attempted and deterministically cannot drop further.
        harness
            .append_message(AgentMessage::user("z".repeat(500_000), vec![]))
            .unwrap();
        let result = harness.prepare_provider_boundary(
            "run-compact",
            boundary_request(false),
            &AgentConfig::default(),
        );
        let error = result.expect_err("strict compaction must remain over budget");
        assert_eq!(
            error,
            "context preparation could not drop historical messages"
        );
        let attempts = harness
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    generation,
                    reason: CompactionReason::AdaptiveBudget,
                    run_id,
                    ..
                } => Some((*generation, run_id.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        // The normal attempt commits generation 1. The exact terminal error
        // proves the second (strict) attempt ran and found no further droppable
        // history; there is no recursive third checkpoint.
        assert_eq!(attempts, vec![(1, "run-compact")]);
    }

    #[test]
    fn provider_overflow_retries_once() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(true),
                &AgentConfig::default(),
            )
            .unwrap();
        let recoveries = harness
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    generation,
                    reason: CompactionReason::OverflowRecovery,
                    run_id,
                    pre_tokens,
                    post_tokens,
                    ..
                } => Some((*generation, run_id.as_str(), *pre_tokens, *post_tokens)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(recoveries.len(), 1);
        assert_eq!(recoveries[0].0, 1);
        assert_eq!(recoveries[0].1, "run-compact");
        assert!(recoveries[0].2 > recoveries[0].3);
        assert_eq!(
            Reducer::reduce(&harness.store)
                .unwrap()
                .lane("main")
                .unwrap()
                .open_operation
                .as_deref(),
            Some("run-compact")
        );
    }

    #[test]
    fn retained_tail_appends_equal_occurrences_without_value_deduplication() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-compact", AgentMessage::user("old", vec![]))
            .unwrap();
        let repeated = AgentMessage::user("repeat", vec![]);
        let summary = AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({ "summary": "summary" }),
        };
        harness
            .commit_prepared_compaction(
                "run-compact",
                "unknown/test-model",
                None,
                &AgentConfig::default(),
                context_budget("unknown/test-model", &AgentConfig::default()),
                CompactionReason::AdaptiveBudget,
                PreparedCompaction {
                    messages: vec![summary, repeated.clone(), repeated.clone()],
                    pre_tokens: 100,
                    post_tokens: 20,
                    compacted_messages: 1,
                    retained_tail_target: 12,
                    retained_tail_tokens: 10,
                },
            )
            .unwrap();

        let context = harness.model_context("main").unwrap().messages();
        assert_eq!(context.len(), 3);
        assert_eq!(context[1], repeated);
        assert_eq!(context[2], repeated);
        let compacted = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    retained_tail_target,
                    retained_tail_tokens,
                    ..
                } => Some((*retained_tail_target, *retained_tail_tokens)),
                _ => None,
            })
            .expect("compaction telemetry");
        assert_eq!(compacted, (12, 10));
        let tail_entries = harness
            .store
            .entries()
            .iter()
            .rev()
            .take(2)
            .collect::<Vec<_>>();
        assert_ne!(tail_entries[0].id, tail_entries[1].id);
        assert_eq!(tail_entries[0].message, tail_entries[1].message);
    }

    #[test]
    fn manual_compaction_telemetry_reports_true_pre_post_and_removed_count() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let config = AgentConfig::default();
        harness
            .begin_run(
                "run",
                AgentMessage::user(format!("old-{}", "x".repeat(4_000)), vec![]),
            )
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("discarded".repeat(500)),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        let before = harness.model_context("main").unwrap().messages();
        let pre_tokens = estimate_request_tokens(&before, None, &config);
        let compacted_messages = 2;
        harness
            .checkpoint_open_run_compaction("run", "durable summary", CompactionReason::Manual)
            .unwrap();
        let tail = AgentMessage::user("tail", vec![]);
        let retained_tail_tokens =
            estimate_request_tokens(std::slice::from_ref(&tail), None, &config);
        harness.append_message_occurrence(tail).unwrap();
        let expected_post = estimate_request_tokens(
            &harness.model_context("main").unwrap().messages(),
            None,
            &config,
        );
        harness
            .record_manual_compaction(
                "run",
                "unknown/test-model",
                &config,
                pre_tokens,
                retained_tail_tokens,
                compacted_messages,
            )
            .unwrap();

        let telemetry = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    reason: CompactionReason::Manual,
                    pre_tokens,
                    post_tokens,
                    retained_tail_tokens,
                    compacted_messages,
                    ..
                } => Some((
                    *pre_tokens,
                    *post_tokens,
                    *retained_tail_tokens,
                    *compacted_messages,
                )),
                _ => None,
            })
            .expect("manual compaction telemetry");
        assert_eq!(
            telemetry,
            (
                pre_tokens,
                expected_post,
                retained_tail_tokens,
                compacted_messages,
            )
        );
        assert!(telemetry.1 < telemetry.0);
    }

    #[test]
    fn cancellation_before_compaction_has_no_partial_operation_or_provider_start() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness.request_abort().unwrap();
        let abort_seq = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::AbortRequested { seq, .. } => Some(*seq),
                _ => None,
            })
            .expect("abort record");
        assert!(harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .is_err());
        assert!(!harness.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ContextCompacted { seq, .. } if *seq > abort_seq)
                || matches!(record, HarnessRecord::ProviderRequestStarted { seq, .. } if *seq > abort_seq)
        }));
    }

    #[test]
    fn cancellation_after_accepted_checkpoint_keeps_complete_canonical_state() {
        let (_dir, path) = temp_session();
        let mut harness = open_long_run(&path);
        harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .unwrap();
        let compacted = harness.model_context("main").unwrap();
        let checkpoint = compacted.checkpoint.clone().expect("accepted checkpoint");
        assert!(
            compacted.messages().len() > 1,
            "retained tail was committed"
        );
        let compacted_seq = harness
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::ContextCompacted { seq, .. } => Some(*seq),
                _ => None,
            })
            .expect("completed compaction telemetry");

        harness.request_abort().unwrap();
        let error = harness
            .prepare_provider_boundary(
                "run-compact",
                boundary_request(false),
                &AgentConfig::default(),
            )
            .expect_err("accepted cancellation blocks subsequent provider preparation");
        assert_eq!(error, "context preparation cancelled");
        assert!(!harness.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ContextCompacted { seq, .. } if *seq > compacted_seq)
                || matches!(record, HarnessRecord::ProviderRequestStarted { .. })
        }));

        drop(harness);
        let reloaded = CodingSessionHarness::open(&path).unwrap();
        let reloaded_context = reloaded.model_context("main").unwrap();
        assert_eq!(reloaded_context.checkpoint, Some(checkpoint));
        let expected_json = serde_json::to_vec(&reloaded_context.messages()).unwrap();
        drop(reloaded);
        let reloaded = CodingSessionHarness::open(&path).unwrap();
        let second_json =
            serde_json::to_vec(&reloaded.model_context("main").unwrap().messages()).unwrap();
        assert_eq!(
            (second_json.len(), Sha256::digest(&second_json)),
            (expected_json.len(), Sha256::digest(&expected_json))
        );
        assert_eq!(
            Reducer::reduce(&reloaded.store)
                .unwrap()
                .lane("main")
                .unwrap()
                .open_operation
                .as_deref(),
            Some("run-compact")
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn path_scoped_async_helper_uses_blocking_worker() {
        let (_dir, path) = temp_session();
        let caller = std::thread::current().id();
        let result = AgentToolResult::external("missing", "read_file", "result", false);

        let _ = CodingSessionHarness::record_tool_result_to_path(&path, "run", &result).await;

        assert_ne!(last_path_operation_thread().unwrap(), caller);
    }

    #[tokio::test]
    async fn tool_intent_precedes_physical_execution_observation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt("run-1").unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "request-1".into(),
                reasoning: None,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{"path":"README.md"}"#.into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();

        CodingSessionHarness::record_tool_execution_to_path(
            &path,
            "run-1",
            ToolExecutionTraceEvent::Started {
                tool_call_id: "call-1".into(),
                tool_name: "read_file".into(),
                executor_kind: "builtin".into(),
                effective_arguments: r#"{"path":"README.md"}"#.into(),
                started_at_ms: 10,
            },
        )
        .await
        .unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let intent_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ToolStarted { seq, .. } => Some(*seq),
            _ => None,
        });
        let observed_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ToolExecutionObserved { seq, .. } => Some(*seq),
            _ => None,
        });
        assert!(matches!(
            (intent_seq, observed_seq),
            (Some(intent), Some(observed)) if intent < observed
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parallel_tool_observations_receive_distinct_sequences() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt("run-1").unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "request-1".into(),
                reasoning: None,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![
                        threadlane_provider::openai::ToolCall {
                            id: "call-1".into(),
                            r#type: "function".into(),
                            function: threadlane_provider::openai::ToolCallFunction {
                                name: "grep_search".into(),
                                arguments: "{}".into(),
                            },
                            thought_signature: None,
                        },
                        threadlane_provider::openai::ToolCall {
                            id: "call-2".into(),
                            r#type: "function".into(),
                            function: threadlane_provider::openai::ToolCallFunction {
                                name: "grep_search".into(),
                                arguments: "{}".into(),
                            },
                            thought_signature: None,
                        },
                    ]),
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();

        let started = |call_id: &str| ToolExecutionTraceEvent::Started {
            tool_call_id: call_id.into(),
            tool_name: "grep_search".into(),
            executor_kind: "builtin".into(),
            started_at_ms: 10,
            effective_arguments: "{}".into(),
        };
        let left_path = path.clone();
        let right_path = path.clone();
        let left = tokio::spawn(async move {
            CodingSessionHarness::record_tool_execution_to_path(
                &left_path,
                "run-1",
                started("call-1"),
            )
            .await
        });
        let right = tokio::spawn(async move {
            CodingSessionHarness::record_tool_execution_to_path(
                &right_path,
                "run-1",
                started("call-2"),
            )
            .await
        });
        left.await.unwrap().unwrap();
        right.await.unwrap().unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let sequences = store
            .records()
            .iter()
            .map(HarnessRecord::seq)
            .collect::<Vec<_>>();
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn provider_attempt_trace_has_one_ordered_terminal_record() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();

        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Started {
                attempt: 1,
                request_id: "request-1".into(),
                model: "test-model".into(),
                provider: "openai".into(),
            },
        )
        .unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Finished {
                attempt: 1,
                request_id: "request-1".into(),
                outcome: threadlane_runtime::harness::ProviderOutcome::Completed,
                error: None,
                duration_ms: 12,
                usage: Some(TokenUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    total_tokens: 5,
                    ..Default::default()
                }),
            },
        )
        .unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let start_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ProviderRequestStarted {
                seq, request_id, ..
            } if request_id.as_ref().map(TraceString::as_str) == Some("request-1") => Some(*seq),
            _ => None,
        });
        let finishes = store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestFinished {
                    seq,
                    request_id,
                    usage,
                    ..
                } if request_id.as_ref().map(TraceString::as_str) == Some("request-1") => {
                    assert_eq!(usage.as_ref().map(|usage| usage.total_tokens), Some(5));
                    Some(*seq)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(finishes.len(), 1);
        assert!(start_seq.is_some_and(|seq| seq < finishes[0]));
    }

    #[test]
    fn cancellation_closes_an_unfinished_provider_attempt_before_abort_observation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Started {
                attempt: 1,
                request_id: "request-1".into(),
                model: "test-model".into(),
                provider: "openai".into(),
            },
        )
        .unwrap();
        let run_id = harness.request_abort().unwrap().unwrap();
        harness.observe_abort_signal(&run_id, true).unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let provider_finish_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ProviderRequestFinished {
                seq,
                outcome: ProviderOutcome::Aborted,
                ..
            } => Some(*seq),
            _ => None,
        });
        let abort_observed_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::AbortObserved { seq, .. } => Some(*seq),
            _ => None,
        });
        assert!(matches!(
            (provider_finish_seq, abort_observed_seq),
            (Some(provider), Some(abort)) if provider < abort
        ));
    }

    #[test]
    fn subagent_start_returns_accepted_child_run_while_main_is_busy() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let parent = harness
            .begin_run("parent-run", AgentMessage::user("parent prompt", vec![]))
            .unwrap();

        let started = harness
            .start_subagent_lane("worker", "inspect", Some(&parent.prompt_entry_id))
            .unwrap();

        assert_eq!(started.accepted.run_id, started.identity.run_id);
        assert_eq!(started.accepted.lane, started.identity.lane_name);
        assert_eq!(
            started.accepted.prompt_entry_id,
            format!("entry-{}-user", started.identity.run_id)
        );
        assert!(started.accepted.accepted_through_seq >= started.identity.started_seq);
        harness.validate_accepted_run(&started.accepted).unwrap();

        let state = Reducer::reduce(harness.store.store()).unwrap();
        assert_eq!(
            state
                .lane("main")
                .and_then(|lane| lane.open_operation.as_deref()),
            Some("parent-run")
        );
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(
                    record,
                    HarnessRecord::OperationStarted { lane, .. } if lane == "main"
                ))
                .count(),
            1
        );
        assert_eq!(
            harness
                .store
                .entries()
                .iter()
                .filter(|entry| entry.lane == "main"
                    && matches!(entry.message, AgentMessage::User { .. }))
                .count(),
            1
        );
    }
    #[test]
    fn subagent_checkpoint_persists_tool_calls_and_results_on_child_lane() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let parent = harness
            .begin_run("parent-run", AgentMessage::user("parent prompt", vec![]))
            .unwrap();
        let started = harness
            .start_subagent_lane("worker", "inspect", Some(&parent.prompt_entry_id))
            .unwrap();
        let call = threadlane_provider::openai::ToolCall {
            id: "shared-call".into(),
            r#type: "function".into(),
            function: threadlane_provider::openai::ToolCallFunction {
                name: "read_file".into(),
                arguments: r#"{"path":"src/lib.rs"}"#.into(),
            },
            thought_signature: None,
        };
        harness
            .checkpoint(
                &started.identity.lane_name,
                &started.identity.run_id,
                &[
                    AgentMessage::Assistant {
                        content: None,
                        tool_calls: Some(vec![call]),
                        stop_reason: None,
                        deferred_handle: None,
                    },
                    AgentMessage::Tool {
                        tool_call_id: "shared-call".into(),
                        name: "read_file".into(),
                        content: "contents".into(),
                        is_error: false,
                        terminate: false,
                    },
                ],
            )
            .unwrap();

        let transcript = harness.store.transcript(&started.identity.lane_name);
        assert!(transcript.entries.iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Assistant { tool_calls: Some(calls), .. }
                if calls.iter().any(|call| call.id == "shared-call")
        )));
        assert!(transcript.entries.iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Tool { tool_call_id, content, .. }
                if tool_call_id == "shared-call" && content == "contents"
        )));
    }

    #[test]
    fn invalid_subagent_source_is_not_retained_for_passive_commit() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let identity = harness
            .start_subagent_lane("worker", "inspect", Some("node_69"))
            .unwrap();

        assert!(identity.identity.source_leaf_id.is_none());
        assert!(harness.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationStarted {
                lane,
                source_leaf_id: None,
                ..
            } if lane == &identity.identity.lane_name
        )));
    }

    // ── No-tool prompt: one OperationStarted + one StepAttempt + one
    //    OperationFinished ──────────────────────────────────────────────
    #[test]
    fn no_tool_prompt_produces_one_operation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();

        // Prepare an assistant attempt
        let _result_entry_id = harness.prepare_assistant_attempt(&run_id).unwrap();

        // Append the assistant message
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("Hello!".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            })
            .unwrap();

        // Record the attempt
        harness
            .record_assistant_attempt(
                &run_id,
                TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    total_tokens: 0,
                },
            )
            .unwrap();

        // Finish
        harness
            .finish_run(&run_id, OperationOutcome::Completed, None)
            .unwrap();

        // Verify records
        let records = harness.store.records();
        let started = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::OperationStarted { .. }))
            .count();
        let attempts = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::StepAttempt { .. }))
            .count();
        let finished = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::OperationFinished { .. }))
            .count();

        assert_eq!(started, 1, "expected exactly one OperationStarted");
        assert_eq!(attempts, 1, "expected exactly one StepAttempt");
        assert_eq!(finished, 1, "expected exactly one OperationFinished");

        // Verify sequences are monotonically increasing
        let seqs: Vec<u64> = records.iter().map(|r| r.seq()).collect();
        for window in seqs.windows(2) {
            assert!(window[0] < window[1], "sequences must increase");
        }
    }

    // ── Reopening produces same reduced main-lane state ──────────────
    #[test]
    fn reopening_produces_same_main_lane_state() {
        let (_dir, path) = temp_session();

        let _run_id = {
            let mut harness = CodingSessionHarness::open(&path).unwrap();
            let id = harness.unique_run_id("test").unwrap();
            harness
                .begin_run(&id, AgentMessage::user("hello", vec![]))
                .unwrap();
            harness.prepare_assistant_attempt(&id).unwrap();
            harness
                .append_message(AgentMessage::Assistant {
                    content: Some("Hi there".into()),
                    tool_calls: None,
                    stop_reason: Some("end_turn".into()),
                    deferred_handle: None,
                })
                .unwrap();
            harness
                .record_assistant_attempt(
                    &id,
                    TokenUsage {
                        input_tokens: 10,
                        output_tokens: 3,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        total_tokens: 0,
                    },
                )
                .unwrap();
            harness
                .finish_run(&id, OperationOutcome::Completed, None)
                .unwrap();
            id
        };

        // Reopen and verify
        let mut reopened = CodingSessionHarness::open(&path).unwrap();
        let state = Reducer::reduce(&reopened.store).unwrap();
        let main_lane = state.lane("main").expect("main lane should exist");

        // The operation should be completed (not open)
        assert!(
            main_lane.open_operation.is_none(),
            "main lane should not have an open operation after finish"
        );

        // Verify the snapshot is consistent
        let snapshot = reopened.snapshot().unwrap();
        let main_snapshot = snapshot
            .state
            .lanes
            .iter()
            .find(|l| l.name == "main")
            .expect("main lane in snapshot");
        assert_eq!(main_snapshot.attempts, 1);
        assert_eq!(main_snapshot.open_operation, None);

        // Verify entries exist
        let entries = reopened.store.entries();
        assert!(
            entries
                .iter()
                .any(|e| matches!(&e.message, AgentMessage::User { .. })),
            "user prompt entry should be present"
        );
        assert!(
            entries
                .iter()
                .any(|e| matches!(&e.message, AgentMessage::Assistant { .. })),
            "assistant entry should be present"
        );
    }

    // ── Error during finish_run propagates correctly ──────────────────
    #[test]
    fn error_during_run_terminates_operation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt(&run_id).unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("error occurred".into()),
                tool_calls: None,
                stop_reason: Some("error".into()),
                deferred_handle: None,
            })
            .unwrap();

        let result = harness.finish_run(
            &run_id,
            OperationOutcome::Failed,
            Some("provider error".into()),
        );
        assert!(result.is_ok(), "finish with error should succeed");

        // Verify the operation is marked as failed
        let state = Reducer::reduce(&harness.store).unwrap();
        let main_lane = state.lane("main").unwrap();
        assert!(main_lane.open_operation.is_none());

        // Verify records show the failure
        let records = harness.store.records();
        let finished_record = records
            .iter()
            .find(|r| matches!(r, HarnessRecord::OperationFinished { .. }));
        assert!(finished_record.is_some(), "should have OperationFinished");
    }

    // Legacy-only reconciliation tests. Production provider execution uses
    // typed append/finish operations and never calls sync_messages.
    // ── Sync messages deduplicates correctly ──────────────────────────
    #[test]
    fn sync_messages_deduplicates_existing_entries() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt(&run_id).unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("response".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            })
            .unwrap();

        let entry_count_before = harness.store.entries().len();

        // Syncing the same messages again should not create duplicates
        harness
            .sync_messages(&[AgentMessage::Assistant {
                content: Some("response".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            }])
            .unwrap();

        assert_eq!(
            harness.store.entries().len(),
            entry_count_before,
            "sync_messages should not create duplicate entries"
        );
    }

    #[tokio::test]
    async fn sync_messages_repairs_a_tool_intent_without_its_result_entry() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();

        harness
            .append_message(AgentMessage::Assistant {
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
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .await
            .unwrap();

        let result = AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "contents".into(),
            is_error: false,
            terminate: false,
        };
        harness.sync_messages(&[result.clone()]).unwrap();

        let state = Reducer::reduce(&harness.store).unwrap();
        assert!(
            state
                .lane("main")
                .unwrap()
                .tools
                .iter()
                .find(|tool| tool.tool_call_id == "call-1")
                .unwrap()
                .completed
        );
    }

    #[test]
    fn sync_messages_persists_provider_visible_queued_user_messages() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let initial = AgentMessage::user("initial", vec![]);
        let queued = AgentMessage::user("queued follow-up", vec![]);

        harness.begin_run(&run_id, initial.clone()).unwrap();
        harness
            .sync_messages(&[initial.clone(), queued.clone()])
            .unwrap();
        harness
            .assert_model_visible(&[initial, queued.clone()])
            .unwrap();

        assert!(harness
            .store
            .model_context("main")
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.message == queued));
    }

    #[test]
    fn model_visibility_rejects_extra_durable_messages() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        let extra = AgentMessage::Assistant {
            content: Some("stale response".into()),
            tool_calls: None,
            stop_reason: Some("end_turn".into()),
            deferred_handle: None,
        };

        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness.sync_messages(&[prompt.clone(), extra]).unwrap();

        let error = harness.assert_model_visible(&[prompt]).unwrap_err();
        assert!(error.contains("durable_count=2, provider_count=1"));
    }

    #[test]
    fn sync_messages_persists_reasoning_before_model_visibility_check() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        let thinking = AgentMessage::Custom {
            custom_type: "thinking".into(),
            payload: serde_json::json!({"text": "reasoning"}),
        };

        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness
            .sync_messages(&[prompt.clone(), thinking.clone()])
            .unwrap();
        harness
            .assert_model_visible(&[prompt, thinking.clone()])
            .unwrap();

        assert!(harness
            .store
            .entries()
            .iter()
            .any(|entry| entry.message == thinking));
    }

    #[tokio::test]
    async fn context_snapshot_capture_indexes_only_successful_local_read_results_once() {
        let (dir, path) = temp_session();
        std::fs::write(dir.path().join("README.md"), "snapshot body").unwrap();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("snapshot").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "read-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{\"path\":\"README.md\",\"start_line\":1,\"end_line\":1}"#
                            .into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "read-1",
                "read_file",
                serde_json::json!({"path": "README.md", "start_line": 1, "end_line": 1}),
            )
            .await
            .unwrap();
        let entry_id = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "read-1".into(),
                name: "read_file".into(),
                content: "snapshot body".into(),
                is_error: false,
                terminate: false,
            })
            .unwrap();

        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "read-1", &entry_id, 13)
                .unwrap(),
            Some("ctx-v2-tool-result-read-1".into())
        );
        assert_eq!(harness.context_snapshots("main").len(), 1);
        assert_eq!(
            harness
                .store
                .entries()
                .iter()
                .filter(|entry| matches!(entry.message, AgentMessage::Tool { .. }))
                .count(),
            1
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "read-1", &entry_id, 13)
                .unwrap(),
            Some("ctx-v2-tool-result-read-1".into())
        );
        let resolved = super::super::context_snapshots::resolve_context_snapshot(
            &path,
            dir.path(),
            "ctx-v2-tool-result-read-1",
        )
        .unwrap();
        assert_eq!(resolved.content, "snapshot body");
        assert_eq!(resolved.snapshot.source_entry_id, entry_id);

        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "missing", &entry_id, 13)
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn compacted_context_snapshot_stays_durable_and_is_indexed_in_checkpoint() {
        let (dir, path) = temp_session();
        std::fs::write(dir.path().join("README.md"), "snapshot body").unwrap();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("snapshot-compact").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "read-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{\"path\":\"README.md\",\"start_line\":1,\"end_line\":1}"#
                            .into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "read-1",
                "read_file",
                serde_json::json!({"path": "README.md", "start_line": 1, "end_line": 1}),
            )
            .await
            .unwrap();
        let source_entry_id = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "read-1".into(),
                name: "read_file".into(),
                content: "snapshot body".into(),
                is_error: false,
                terminate: false,
            })
            .unwrap();
        let context_id = harness
            .index_read_snapshot(&run_id, dir.path(), "read-1", &source_entry_id, 13)
            .unwrap()
            .unwrap();
        harness
            .append_message(AgentMessage::user("continue", vec![]))
            .unwrap();
        let config = AgentConfig::builder().max_checkpoint_chars(0).build();
        let before = harness.model_context("main").unwrap().messages();
        let prepared = compact_for_budget(&before, None, 1, &config).unwrap();
        harness
            .commit_prepared_compaction(
                &run_id,
                "unknown/test-model",
                None,
                &config,
                context_budget("unknown/test-model", &config),
                CompactionReason::AdaptiveBudget,
                prepared,
            )
            .unwrap();

        let compacted_messages = harness.model_context("main").unwrap().messages();
        let checkpoint = compacted_messages
            .iter()
            .find_map(threadlane_runtime::compaction_summary_text)
            .unwrap();
        assert!(checkpoint.contains(&context_id));
        assert!(checkpoint.contains("README.md:1-1 sha256="));
        assert!(!checkpoint.contains("snapshot body"));
        assert!(!compacted_messages
            .iter()
            .any(|message| matches!(message, AgentMessage::Tool { content, .. } if content == "snapshot body")));
        assert!(JsonlStore::open(&path)
            .unwrap()
            .entries()
            .iter()
            .any(|entry| entry.id == source_entry_id && matches!(&entry.message, AgentMessage::Tool { content, .. } if content == "snapshot body")));
        assert_eq!(
            super::super::context_snapshots::resolve_context_snapshot(
                &path,
                dir.path(),
                &context_id
            )
            .unwrap()
            .content,
            "snapshot body"
        );
        let post_tokens = estimate_request_tokens(&compacted_messages, None, &config);
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .find_map(|record| match record {
                    HarnessRecord::ContextCompacted { post_tokens, .. } => Some(*post_tokens),
                    _ => None,
                }),
            Some(post_tokens)
        );
    }

    #[tokio::test]
    async fn context_snapshot_capture_skips_failed_virtual_and_unrecorded_reads() {
        let (dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("snapshot-skip").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![
                    threadlane_provider::openai::ToolCall {
                        id: "failed".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{\"path\":\"README.md\"}"#.into(),
                        },
                        thought_signature: None,
                    },
                    threadlane_provider::openai::ToolCall {
                        id: "virtual".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{\"path\":\"virtual://README.md\"}"#.into(),
                        },
                        thought_signature: None,
                    },
                    threadlane_provider::openai::ToolCall {
                        id: "remote".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{\"path\":\"https:README.md\"}"#.into(),
                        },
                        thought_signature: None,
                    },
                ]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        for (call_id, arguments) in [
            ("failed", serde_json::json!({"path": "README.md"})),
            (
                "virtual",
                serde_json::json!({"path": "virtual://README.md"}),
            ),
            ("remote", serde_json::json!({"path": "https:README.md"})),
        ] {
            harness
                .append_tool_intent(&run_id, call_id, "read_file", arguments)
                .await
                .unwrap();
        }
        let failed_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "failed".into(),
                name: "read_file".into(),
                content: "not found".into(),
                is_error: true,
                terminate: false,
            })
            .unwrap();
        let virtual_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "virtual".into(),
                name: "read_file".into(),
                content: "body".into(),
                is_error: false,
                terminate: false,
            })
            .unwrap();
        let remote_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "remote".into(),
                name: "read_file".into(),
                content: "body".into(),
                is_error: false,
                terminate: false,
            })
            .unwrap();
        let unrecorded_entry = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "unrecorded".into(),
                name: "read_file".into(),
                content: "body".into(),
                is_error: false,
                terminate: false,
            })
            .unwrap();

        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "failed", &failed_entry, 9)
                .unwrap(),
            None
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "virtual", &virtual_entry, 4)
                .unwrap(),
            None
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "remote", &remote_entry, 4)
                .unwrap(),
            None
        );
        assert_eq!(
            harness
                .index_read_snapshot(&run_id, dir.path(), "unrecorded", &unrecorded_entry, 4)
                .unwrap(),
            None
        );
        assert!(harness.context_snapshots("main").is_empty());
    }

    #[test]
    fn stale_metadata_and_chained_tool_results_remain_model_visible_and_get_lifecycle_records() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness.prepare_assistant_attempt(&run_id).unwrap();

        let mut stale_store = threadlane_runtime::harness::JsonlStore::open(&path).unwrap();
        stale_store.set_name("stale metadata").unwrap();

        let assistant = AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![
                threadlane_provider::openai::ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                },
                threadlane_provider::openai::ToolCall {
                    id: "call-2".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "grep".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                },
            ]),
            stop_reason: None,
            deferred_handle: None,
        };
        let first_tool = AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "first".into(),
            is_error: false,
            terminate: false,
        };
        let second_tool = AgentMessage::Tool {
            tool_call_id: "call-2".into(),
            name: "grep".into(),
            content: "second".into(),
            is_error: false,
            terminate: false,
        };
        let final_assistant = AgentMessage::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            stop_reason: Some("end_turn".into()),
            deferred_handle: None,
        };
        let messages = vec![prompt, assistant, first_tool, second_tool, final_assistant];

        harness.sync_messages(&messages).unwrap();
        harness.assert_model_visible(&messages).unwrap();
        harness
            .record_completed_tools_with_termination(&run_id, &HashMap::new())
            .unwrap();

        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::ToolStarted { .. }))
                .count(),
            2
        );
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::ToolFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn sync_messages_persists_identical_empty_assistant_results_for_each_run() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let empty_assistant = AgentMessage::Assistant {
            content: None,
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        };
        let mut provider_messages = Vec::new();

        for prompt_text in ["first prompt", "second prompt"] {
            let run_id = harness.unique_run_id("test").unwrap();
            let prompt = AgentMessage::user(prompt_text, vec![]);
            harness.begin_run(&run_id, prompt.clone()).unwrap();
            provider_messages.push(prompt);
            provider_messages.push(empty_assistant.clone());

            // This mirrors CodingAgent's full provider-state synchronization
            // after each prompt. The second empty assistant must be a new
            // durable entry even though its content matches the first one.
            harness.sync_messages(&provider_messages).unwrap();
            harness
                .record_assistant_attempt(&run_id, TokenUsage::default())
                .unwrap();
            harness
                .finish_run(&run_id, OperationOutcome::Completed, None)
                .unwrap();
        }

        let assistant_entries: Vec<_> = harness
            .store
            .entries()
            .iter()
            .filter(|entry| matches!(entry.message, AgentMessage::Assistant { .. }))
            .collect();
        assert_eq!(assistant_entries.len(), 2);
        assert_ne!(assistant_entries[0].id, assistant_entries[1].id);
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::OperationFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn assistant_ready_persists_provider_response_attached_record() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("test prompt", vec![]))
            .unwrap();

        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            &run_id,
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "req-123".into(),
                reasoning: Some("deep thinking".into()),
                message: AgentMessage::Assistant {
                    content: Some("final answer".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            &run_id,
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "req-123-retry".into(),
                reasoning: Some("deep thinking".into()),
                message: AgentMessage::Assistant {
                    content: Some("final answer".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();

        let updated_harness = CodingSessionHarness::open(&path).unwrap();
        assert_eq!(
            updated_harness
                .store
                .entries()
                .iter()
                .filter(|entry| matches!(entry.message, AgentMessage::Assistant { .. }))
                .count(),
            1
        );
        let response_record = updated_harness
            .store
            .records()
            .iter()
            .find(|record| matches!(record, HarnessRecord::ProviderResponseAttached { .. }))
            .expect("must record ProviderResponseAttached");

        if let HarnessRecord::ProviderResponseAttached {
            run_id: rec_run_id,
            attempt,
            request_id,
            entry_id,
            reasoning_entry_id,
            ..
        } = response_record
        {
            assert_eq!(rec_run_id, &run_id);
            assert_eq!(*attempt, 1);
            assert_eq!(request_id.as_ref().map(|r| r.as_str()), Some("req-123"));
            assert!(!entry_id.is_empty());
            assert!(reasoning_entry_id.is_some());
        } else {
            panic!("expected ProviderResponseAttached");
        }
    }
}
