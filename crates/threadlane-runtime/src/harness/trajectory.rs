//! Strongly-typed trajectory projection engine for the Threadlane session journal.
//!
//! Provides structured, joined representations of requests, provider requests,
//! exact context manifests, tool execution lifecycles, and causal reference chains.

use super::{
    ContextManifestItem, Entry, PermissionTraceDecision, PermissionTraceScope,
    PermissionTraceSource, ProviderErrorSummary, ProviderOutcome, Record, SessionStore,
    SubagentLifecyclePhase, ToolExecutionOutcome, ToolExecutionPhase,
};
use crate::types::{AgentMessage, TokenUsage};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A stable pointer to an entry or record in the session journal.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrajectoryRef {
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    lane: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestStatus {
    InProgress,
    Completed,
    Failed,
    Interrupted,
    AwaitingApproval,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestTrajectory {
    request_index: u32,
    prompt_entry_id: String,
    prompt_text: String,
    started_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    status: RequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_assistant_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_assistant_text: Option<String>,
    turn_count: u32,
    tool_calls_count: u32,
    files_mutated: Vec<String>,
    commands_executed: Vec<String>,
    usage: TokenUsage,
    root_ref: TrajectoryRef,
    item_refs: Vec<TrajectoryRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextManifestTrajectory {
    request_id: String,
    attempt: u32,
    seq: u64,
    timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total_estimated_tokens: Option<u32>,
    items: Vec<ContextManifestItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_ref: Option<TrajectoryRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshotCaptureTrajectory {
    pub seq: u64,
    pub context_id: String,
    pub source_lane: String,
    pub source_run_id: String,
    pub source_tool_call_id: String,
    pub source_entry_id: String,
    pub path: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub file_sha256: String,
    pub output_chars: usize,
    pub captured_at: u64,
    pub duplicate_candidate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshotLoadTrajectory {
    pub seq: u64,
    pub context_id: String,
    pub source_lane: String,
    pub requesting_lane: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub outcome: super::ContextSnapshotLoadOutcome,
    pub duplicate_candidate: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderTrajectory {
    attempt: u32,
    request_id: String,
    provider: String,
    model: String,
    started_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<ProviderOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<ProviderErrorSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_manifest_ref: Option<TrajectoryRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_entry: Option<TrajectoryRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response_entry_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Started,
    Executing,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolTrajectory {
    call_id: String,
    tool_name: String,
    effective_args: serde_json::Value,
    status: ToolStatus,
    started_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    executed_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_content: Option<String>,
    is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_assistant_entry: Option<TrajectoryRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_entry: Option<TrajectoryRef>,
    raw_record_seqs: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionTrajectory {
    request_id: String,
    capability: String,
    scopes: Vec<PermissionTraceScope>,
    requested_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<PermissionTraceDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    source: PermissionTraceSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentTrajectory {
    child_run_id: String,
    agent_id: String,
    subagent_lane: String,
    phase: SubagentLifecyclePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    seq: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenericDurableTrajectory {
    seq: u64,
    id: String,
    lane: String,
    category: String,
    summary: String,
    detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnomalyKind {
    RepeatedToolCallIdenticalArgs,
    ProviderRetryLoop,
    OrphanedToolStart,
    ContextOverflowRisk,
    /// Consecutive failures of one tool with the same output hash (mirrors
    /// the runtime loop guard's error-loop tripwire).
    ErrorLoop,
    /// Consecutive A→B→A… tool cycles with stable outputs (mirrors the
    /// runtime ping-pong tripwire).
    PingPongCycle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticAnomaly {
    kind: AnomalyKind,
    pub summary: String,
    pub description: String,
    pub related_refs: Vec<TrajectoryRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TrajectoryItem {
    Request(RequestTrajectory),
    ContextManifest(ContextManifestTrajectory),
    ContextSnapshotCapture(ContextSnapshotCaptureTrajectory),
    ContextSnapshotLoad(ContextSnapshotLoadTrajectory),
    Provider(ProviderTrajectory),
    Tool(ToolTrajectory),
    Permission(PermissionTrajectory),
    Subagent(SubagentTrajectory),
    Anomaly(DiagnosticAnomaly),
    Event(GenericDurableTrajectory),
}

impl TrajectoryItem {
    fn seq(&self) -> u64 {
        match self {
            Self::Request(r) => r.started_seq,
            Self::ContextManifest(c) => c.seq,
            Self::ContextSnapshotCapture(c) => c.seq,
            Self::ContextSnapshotLoad(c) => c.seq,
            Self::Provider(p) => p.started_seq,
            Self::Tool(t) => t.started_seq,
            Self::Permission(p) => p.requested_seq,
            Self::Subagent(s) => s.seq,
            Self::Anomaly(a) => a.related_refs.first().map_or(0, |r| r.seq),
            Self::Event(e) => e.seq,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionTrajectory {
    requests: Vec<RequestTrajectory>,
    items: Vec<TrajectoryItem>,
    pub anomalies: Vec<DiagnosticAnomaly>,
    total_usage: TokenUsage,
}

fn extract_mutated_files(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    if matches!(
        tool_name,
        "write_file"
            | "create_file"
            | "replace_file_content"
            | "multi_replace_file_content"
            | "edit_file_hashline"
            | "edit_files_hashline"
            | "apply_workspace_edit_plan"
    ) {
        args.get("path")
            .or_else(|| args.get("file_path"))
            .or_else(|| args.get("TargetFile"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    } else {
        None
    }
}

fn extract_command(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    if matches!(tool_name, "run_command" | "execute" | "bash") {
        args.get("command")
            .or_else(|| args.get("CommandLine"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    } else {
        None
    }
}

/// Projects a complete typed trajectory from any session store.
pub fn project_trajectory<S: SessionStore>(store: &S) -> SessionTrajectory {
    let mut total_usage = TokenUsage::default();
    let mut items: Vec<TrajectoryItem> = Vec::new();

    // Intermediate indices for lifecycle joining
    let mut tools_by_call_id: HashMap<String, ToolTrajectory> = HashMap::new();
    let mut providers_by_req_id: HashMap<String, ProviderTrajectory> = HashMap::new();
    let mut context_manifests_by_req_id: HashMap<String, ContextManifestTrajectory> =
        HashMap::new();
    let mut permissions_by_req_id: HashMap<String, PermissionTrajectory> = HashMap::new();
    let mut context_snapshots_by_id = HashMap::new();
    let mut duplicate_candidate_by_context_id = HashMap::new();
    let mut seen_snapshot_captures = HashSet::new();

    let mut requests: Vec<RequestTrajectory> = Vec::new();
    let mut current_request: Option<RequestTrajectory> = None;
    let mut request_index = 0u32;
    // Per-request dedup sets for mutated files/commands (O(1) instead of a
    // linear `contains` scan per tool). Reset whenever a new request opens.
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut seen_commands: HashSet<String> = HashSet::new();

    // Merge the two seq-ordered streams in linear time instead of
    // concatenating and re-sorting (O(n log n)). All stores keep entries and
    // records in seq order (JsonlStore assigns at append and sorts records on
    // load; SqliteStore loads ORDER BY seq), with entries winning ties to
    // match the previous stable sort.
    #[derive(Clone)]
    enum JournalItem<'a> {
        Entry(&'a Entry),
        Record(&'a Record),
    }
    #[cfg(debug_assertions)]
    {
        debug_assert!(store.entries().windows(2).all(|w| w[0].seq <= w[1].seq));
        debug_assert!(store.records().windows(2).all(|w| {
            w[0].seq() <= w[1].seq()
        }));
    }
    let mut journal: Vec<JournalItem> = Vec::with_capacity(store.entries().len() + store.records().len());
    {
        let mut entries = store.entries().iter().peekable();
        let mut records = store.records().iter().peekable();
        loop {
            let take_entry = match (entries.peek(), records.peek()) {
                (Some(e), Some(r)) => e.seq <= r.seq(),
                (Some(_), None) => true,
                (None, _) => false,
            };
            if take_entry {
                journal.push(JournalItem::Entry(entries.next().expect("peeked entry")));
            } else if records.peek().is_some() {
                journal.push(JournalItem::Record(records.next().expect("peeked record")));
            } else {
                break;
            }
        }
    }

    for item in journal {
        match item {
            JournalItem::Entry(entry) => {
                match &entry.message {
                    AgentMessage::User { content }
                    | AgentMessage::UserWithImages { content, .. } => {
                        // Close prior request if open
                        if let Some(mut req) = current_request.take() {
                            req.status = RequestStatus::Completed;
                            requests.push(req);
                        }
                        seen_files.clear();
                        seen_commands.clear();
                        request_index += 1;
                        let root_ref = TrajectoryRef {
                            seq: entry.seq,
                            entry_id: Some(entry.id.clone()),
                            run_id: None,
                            lane: entry.lane.clone(),
                        };
                        current_request = Some(RequestTrajectory {
                            request_index,
                            prompt_entry_id: entry.id.clone(),
                            prompt_text: content.clone(),
                            started_seq: entry.seq,
                            finished_seq: None,
                            duration_ms: None,
                            status: RequestStatus::InProgress,
                            final_assistant_entry_id: None,
                            final_assistant_text: None,
                            turn_count: 0,
                            tool_calls_count: 0,
                            files_mutated: Vec::new(),
                            commands_executed: Vec::new(),
                            usage: TokenUsage::default(),
                            root_ref: root_ref.clone(),
                            item_refs: vec![root_ref],
                        });
                    }
                    AgentMessage::Assistant {
                        content,
                        tool_calls,
                        ..
                    } => {
                        if let Some(req) = current_request.as_mut() {
                            req.final_assistant_entry_id = Some(entry.id.clone());
                            if let Some(c) = content {
                                req.final_assistant_text = Some(c.clone());
                            }
                            req.item_refs.push(TrajectoryRef {
                                seq: entry.seq,
                                entry_id: Some(entry.id.clone()),
                                run_id: None,
                                lane: entry.lane.clone(),
                            });
                        }
                        // Associate tool calls with this assistant entry
                        if let Some(calls) = tool_calls {
                            for call in calls {
                                let tool =
                                    tools_by_call_id.entry(call.id.clone()).or_insert_with(|| {
                                        let args = serde_json::from_str(&call.function.arguments)
                                            .unwrap_or_else(|_| {
                                                serde_json::Value::String(
                                                    call.function.arguments.clone(),
                                                )
                                            });
                                        ToolTrajectory {
                                            call_id: call.id.clone(),
                                            tool_name: call.function.name.clone(),
                                            effective_args: args,
                                            status: ToolStatus::Started,
                                            started_seq: entry.seq,
                                            executed_seq: None,
                                            finished_seq: None,
                                            duration_ms: None,
                                            exit_code: None,
                                            output_bytes: None,
                                            output_sha256: None,
                                            output_summary: None,
                                            result_content: None,
                                            is_error: false,
                                            parent_assistant_entry: Some(TrajectoryRef {
                                                seq: entry.seq,
                                                entry_id: Some(entry.id.clone()),
                                                run_id: None,
                                                lane: entry.lane.clone(),
                                            }),
                                            result_entry: None,
                                            raw_record_seqs: Vec::new(),
                                        }
                                    });
                                tool.parent_assistant_entry = Some(TrajectoryRef {
                                    seq: entry.seq,
                                    entry_id: Some(entry.id.clone()),
                                    run_id: None,
                                    lane: entry.lane.clone(),
                                });
                            }
                        }
                    }
                    AgentMessage::Tool {
                        tool_call_id,
                        content,
                        is_error,
                        ..
                    } => {
                        let tool =
                            tools_by_call_id
                                .entry(tool_call_id.clone())
                                .or_insert_with(|| ToolTrajectory {
                                    call_id: tool_call_id.clone(),
                                    tool_name: "tool".into(),
                                    effective_args: serde_json::Value::Null,
                                    status: if *is_error {
                                        ToolStatus::Failed
                                    } else {
                                        ToolStatus::Succeeded
                                    },
                                    started_seq: entry.seq,
                                    executed_seq: None,
                                    finished_seq: Some(entry.seq),
                                    duration_ms: None,
                                    exit_code: None,
                                    output_bytes: Some(content.len() as u64),
                                    output_sha256: None,
                                    output_summary: None,
                                    result_content: Some(content.clone()),
                                    is_error: *is_error,
                                    parent_assistant_entry: None,
                                    result_entry: Some(TrajectoryRef {
                                        seq: entry.seq,
                                        entry_id: Some(entry.id.clone()),
                                        run_id: None,
                                        lane: entry.lane.clone(),
                                    }),
                                    raw_record_seqs: Vec::new(),
                                });
                        tool.result_content = Some(content.clone());
                        tool.is_error = *is_error;
                        tool.status = if *is_error {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Succeeded
                        };
                        tool.result_entry = Some(TrajectoryRef {
                            seq: entry.seq,
                            entry_id: Some(entry.id.clone()),
                            run_id: None,
                            lane: entry.lane.clone(),
                        });
                    }
                    _ => {}
                }
            }
            JournalItem::Record(record) => {
                let seq = record.seq();
                let lane = record.lane().to_string();
                let run_id = record.run_id().map(str::to_owned);

                if let Some(req) = current_request.as_mut() {
                    req.item_refs.push(TrajectoryRef {
                        seq,
                        entry_id: None,
                        run_id: run_id.clone(),
                        lane: lane.clone(),
                    });
                }

                match record {
                    Record::StepAttempt { attempt, .. } => {
                        if let Some(req) = current_request.as_mut() {
                            req.turn_count = req.turn_count.max(*attempt);
                        }
                    }
                    Record::ContextManifestCaptured {
                        seq,
                        attempt,
                        request_id,
                        total_estimated_tokens,
                        items,
                        timestamp,
                        ..
                    } => {
                        let manifest = ContextManifestTrajectory {
                            request_id: request_id.as_str().to_owned(),
                            attempt: *attempt,
                            seq: *seq,
                            timestamp: *timestamp,
                            total_estimated_tokens: *total_estimated_tokens,
                            items: items.clone(),
                            parent_ref: current_request.as_ref().map(|r| r.root_ref.clone()),
                        };
                        context_manifests_by_req_id
                            .insert(request_id.as_str().to_owned(), manifest);
                    }
                    Record::ContextSnapshotIndexed { seq, snapshot, .. } => {
                        let capture = (
                            snapshot.source_lane.clone(),
                            snapshot.path.clone(),
                            snapshot.start_line,
                            snapshot.end_line,
                            snapshot.file_sha256.as_str().to_owned(),
                        );
                        let duplicate_candidate = !seen_snapshot_captures.insert(capture);
                        duplicate_candidate_by_context_id
                            .insert(snapshot.context_id.clone(), duplicate_candidate);
                        context_snapshots_by_id
                            .insert(snapshot.context_id.clone(), snapshot.clone());
                        items.push(TrajectoryItem::ContextSnapshotCapture(
                            ContextSnapshotCaptureTrajectory {
                                seq: *seq,
                                context_id: snapshot.context_id.clone(),
                                source_lane: snapshot.source_lane.clone(),
                                source_run_id: snapshot.source_run_id.clone(),
                                source_tool_call_id: snapshot.source_tool_call_id.clone(),
                                source_entry_id: snapshot.source_entry_id.clone(),
                                path: snapshot.path.clone(),
                                start_line: snapshot.start_line,
                                end_line: snapshot.end_line,
                                file_sha256: snapshot.file_sha256.as_str().to_owned(),
                                output_chars: snapshot.output_chars,
                                captured_at: snapshot.captured_at,
                                duplicate_candidate,
                            },
                        ));
                    }
                    Record::ContextSnapshotLoaded {
                        seq,
                        context_id,
                        source_lane,
                        outcome,
                        ..
                    } => {
                        let snapshot = context_snapshots_by_id.get(context_id);
                        items.push(TrajectoryItem::ContextSnapshotLoad(
                            ContextSnapshotLoadTrajectory {
                                seq: *seq,
                                context_id: context_id.clone(),
                                source_lane: source_lane.clone(),
                                requesting_lane: lane.clone(),
                                path: snapshot.map(|snapshot| snapshot.path.clone()),
                                start_line: snapshot.and_then(|snapshot| snapshot.start_line),
                                end_line: snapshot.and_then(|snapshot| snapshot.end_line),
                                outcome: *outcome,
                                duplicate_candidate: duplicate_candidate_by_context_id
                                    .get(context_id)
                                    .copied()
                                    .unwrap_or(false),
                            },
                        ));
                    }
                    Record::ProviderRequestStarted {
                        seq,
                        attempt,
                        provider,
                        model,
                        request_id,
                        ..
                    } => {
                        let req_id_str = request_id
                            .as_ref()
                            .map_or_else(|| format!("req-{seq}"), |id| id.as_str().to_owned());
                        let manifest_ref =
                            context_manifests_by_req_id
                                .get(&req_id_str)
                                .map(|m| TrajectoryRef {
                                    seq: m.seq,
                                    entry_id: None,
                                    run_id: run_id.clone(),
                                    lane: lane.clone(),
                                });
                        providers_by_req_id.insert(
                            req_id_str.clone(),
                            ProviderTrajectory {
                                attempt: *attempt,
                                request_id: req_id_str,
                                provider: provider.as_str().to_owned(),
                                model: model.as_str().to_owned(),
                                started_seq: *seq,
                                finished_seq: None,
                                duration_ms: None,
                                outcome: None,
                                error: None,
                                usage: None,
                                context_manifest_ref: manifest_ref,
                                parent_entry: current_request.as_ref().map(|r| r.root_ref.clone()),
                                response_entry_id: None,
                            },
                        );
                    }
                    Record::ProviderRequestFinished {
                        seq,
                        request_id,
                        outcome,
                        error,
                        duration_ms,
                        usage,
                        ..
                    } => {
                        if let Some(u) = usage {
                            total_usage.accumulate(u);
                            if let Some(req) = current_request.as_mut() {
                                req.usage.accumulate(u);
                            }
                        }
                        if let Some(req_id) = request_id {
                            if let Some(provider) = providers_by_req_id.get_mut(req_id.as_str()) {
                                provider.finished_seq = Some(*seq);
                                provider.outcome = Some(outcome.clone());
                                provider.error = error.clone();
                                provider.duration_ms = *duration_ms;
                                provider.usage = usage.clone();
                            }
                        }
                    }
                    Record::ProviderResponseAttached {
                        request_id,
                        entry_id,
                        ..
                    } => {
                        if let Some(req_id) = request_id {
                            if let Some(provider) = providers_by_req_id.get_mut(req_id.as_str()) {
                                provider.response_entry_id = Some(entry_id.clone());
                            }
                        }
                    }
                    Record::ToolStarted {
                        seq,
                        tool_call_id,
                        tool_name,
                        effective_args,
                        replay: _,
                        assistant_entry_id,
                        ..
                    } => {
                        if let Some(req) = current_request.as_mut() {
                            req.tool_calls_count += 1;
                            if let Some(mutated) = extract_mutated_files(tool_name, effective_args)
                            {
                                if seen_files.insert(mutated.clone()) {
                                    req.files_mutated.push(mutated);
                                }
                            }
                            if let Some(cmd) = extract_command(tool_name, effective_args) {
                                if seen_commands.insert(cmd.clone()) {
                                    req.commands_executed.push(cmd);
                                }
                            }
                        }
                        let tool =
                            tools_by_call_id
                                .entry(tool_call_id.clone())
                                .or_insert_with(|| ToolTrajectory {
                                    call_id: tool_call_id.clone(),
                                    tool_name: tool_name.clone(),
                                    effective_args: effective_args.clone(),
                                    status: ToolStatus::Started,
                                    started_seq: *seq,
                                    executed_seq: None,
                                    finished_seq: None,
                                    duration_ms: None,
                                    exit_code: None,
                                    output_bytes: None,
                                    output_sha256: None,
                                    output_summary: None,
                                    result_content: None,
                                    is_error: false,
                                    parent_assistant_entry: Some(TrajectoryRef {
                                        seq: *seq,
                                        entry_id: Some(assistant_entry_id.clone()),
                                        run_id: run_id.clone(),
                                        lane: lane.clone(),
                                    }),
                                    result_entry: None,
                                    raw_record_seqs: vec![*seq],
                                });
                        tool.raw_record_seqs.push(*seq);
                    }
                    Record::ToolExecutionObserved {
                        seq,
                        tool_call_id,
                        phase,
                        duration_ms,
                        exit_code,
                        output_bytes,
                        output_sha256,
                        outcome,
                        ..
                    } => {
                        if let Some(tool) = tools_by_call_id.get_mut(tool_call_id.as_str()) {
                            tool.raw_record_seqs.push(*seq);
                            match phase {
                                ToolExecutionPhase::Started | ToolExecutionPhase::Progress => {
                                    if tool.executed_seq.is_none() {
                                        tool.executed_seq = Some(*seq);
                                    }
                                    tool.status = ToolStatus::Executing;
                                }
                                ToolExecutionPhase::Finished => {
                                    tool.finished_seq = Some(*seq);
                                    if let Some(dur) = duration_ms {
                                        tool.duration_ms = Some(*dur);
                                    }
                                    tool.exit_code = *exit_code;
                                    tool.output_bytes = *output_bytes;
                                    tool.output_sha256 =
                                        output_sha256.as_ref().map(|s| s.as_str().to_owned());
                                    if let Some(out) = outcome {
                                        tool.status = match out {
                                            ToolExecutionOutcome::Succeeded => {
                                                ToolStatus::Succeeded
                                            }
                                            ToolExecutionOutcome::Failed => ToolStatus::Failed,
                                            ToolExecutionOutcome::Cancelled
                                            | ToolExecutionOutcome::Declined => {
                                                ToolStatus::Interrupted
                                            }
                                        };
                                    }
                                }
                            }
                        }
                    }
                    Record::ToolFinished {
                        seq, tool_call_id, ..
                    } => {
                        if let Some(tool) = tools_by_call_id.get_mut(tool_call_id) {
                            tool.raw_record_seqs.push(*seq);
                            if tool.finished_seq.is_none() {
                                tool.finished_seq = Some(*seq);
                            }
                        }
                    }
                    Record::PermissionRequested {
                        seq,
                        request_id,
                        capability,
                        scopes,
                        source,
                        ..
                    } => {
                        permissions_by_req_id.insert(
                            request_id.as_str().to_owned(),
                            PermissionTrajectory {
                                request_id: request_id.as_str().to_owned(),
                                capability: capability.as_str().to_owned(),
                                scopes: scopes.clone(),
                                requested_seq: *seq,
                                resolved_seq: None,
                                decision: None,
                                duration_ms: None,
                                source: source.clone(),
                            },
                        );
                    }
                    Record::PermissionResolved {
                        seq,
                        request_id,
                        decision,
                        ..
                    } => {
                        if let Some(perm) = permissions_by_req_id.get_mut(request_id.as_str()) {
                            perm.resolved_seq = Some(*seq);
                            perm.decision = Some(decision.clone());
                        }
                    }
                    Record::SubagentLifecycle {
                        seq,
                        child_run_id,
                        agent_id,
                        subagent_lane,
                        phase,
                        parent_tool_call_id,
                        result_entry_id,
                        error,
                        ..
                    } => {
                        items.push(TrajectoryItem::Subagent(SubagentTrajectory {
                            child_run_id: child_run_id.as_str().to_owned(),
                            agent_id: agent_id.as_str().to_owned(),
                            subagent_lane: subagent_lane.as_str().to_owned(),
                            phase: phase.clone(),
                            parent_tool_call_id: parent_tool_call_id
                                .as_ref()
                                .map(|s| s.as_str().to_owned()),
                            result_entry_id: result_entry_id
                                .as_ref()
                                .map(|s| s.as_str().to_owned()),
                            error: error.as_ref().map(|s| s.as_str().to_owned()),
                            seq: *seq,
                        }));
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(mut req) = current_request.take() {
        req.status = RequestStatus::Completed;
        requests.push(req);
    }

    // Canonical journals append ProviderRequestStarted before the manifest.
    // Backfill the join after the scan; the eager lookup above remains useful
    // for legacy manifest-before-start journals.
    for (request_id, provider) in &mut providers_by_req_id {
        if provider.context_manifest_ref.is_none() {
            provider.context_manifest_ref =
                context_manifests_by_req_id
                    .get(request_id)
                    .map(|manifest| TrajectoryRef {
                        seq: manifest.seq,
                        entry_id: None,
                        run_id: manifest
                            .parent_ref
                            .as_ref()
                            .and_then(|reference| reference.run_id.clone()),
                        lane: manifest
                            .parent_ref
                            .as_ref()
                            .map_or_else(|| "main".to_owned(), |reference| reference.lane.clone()),
                    });
        }
    }

    // Populate joined items
    for (_, manifest) in context_manifests_by_req_id {
        items.push(TrajectoryItem::ContextManifest(manifest));
    }
    for (_, provider) in providers_by_req_id {
        items.push(TrajectoryItem::Provider(provider));
    }
    for (_, tool) in tools_by_call_id {
        items.push(TrajectoryItem::Tool(tool));
    }
    for (_, perm) in permissions_by_req_id {
        items.push(TrajectoryItem::Permission(perm));
    }
    for req in &requests {
        items.push(TrajectoryItem::Request(req.clone()));
    }

    // Sort items chronologically by sequence
    items.sort_by_key(TrajectoryItem::seq);

    // ── Diagnostic Anomaly Detection Pass ──────────────────────────────────
    let mut anomalies = Vec::new();

    /// Minimum consecutive same-error runs for an ErrorLoop anomaly (mirrors
    /// the runtime loop guard default).
    const ERROR_LOOP_MIN_RUN: usize = 3;
    /// Minimum consecutive cycle rounds for a PingPongCycle anomaly.
    const PING_PONG_MIN_ROUNDS: usize = 3;

    // 1. Detect repeated tool calls with identical arguments (potential execution loop)
    let mut seen_calls: HashMap<(String, String), Vec<TrajectoryRef>> = HashMap::new();
    for item in &items {
        if let TrajectoryItem::Tool(tool) = item {
            let key = (tool.tool_name.clone(), tool.effective_args.to_string());
            seen_calls.entry(key).or_default().push(TrajectoryRef {
                seq: tool.started_seq,
                entry_id: None,
                run_id: None,
                lane: "main".into(),
            });
        }
    }
    for ((name, args_str), refs) in seen_calls {
        if refs.len() >= 3 {
            anomalies.push(DiagnosticAnomaly {
                kind: AnomalyKind::RepeatedToolCallIdenticalArgs,
                summary: format!("Repeated tool invocation: '{name}' called {} times with identical arguments", refs.len()),
                description: format!("Tool '{name}' was invoked {} times with identical parameters:\n```json\n{args_str}\n```", refs.len()),
                related_refs: refs,
            });
        }
    }

    // 2. Detect orphaned tool starts (started without execution or result)
    for item in &items {
        if let TrajectoryItem::Tool(tool) = item {
            if tool.status == ToolStatus::Started && tool.finished_seq.is_none() {
                anomalies.push(DiagnosticAnomaly {
                    kind: AnomalyKind::OrphanedToolStart,
                    summary: format!("Orphaned tool start for '{}'", tool.tool_name),
                    description: format!("Tool '{}' (call ID {}) was started but never recorded completion or failure.", tool.tool_name, tool.call_id),
                    related_refs: vec![TrajectoryRef {
                        seq: tool.started_seq,
                        entry_id: None,
                        run_id: None,
                        lane: "main".into(),
                    }],
                });
            }
        }
    }

    // 3. Detect consecutive same-error runs (mirrors the runtime error-loop
    // tripwire): one tool failing repeatedly with the same output hash.
    // Arguments may vary; the error signature must match consecutively.
    {
        fn error_signature(tool: &ToolTrajectory) -> Option<String> {
            (tool.status == ToolStatus::Failed).then(|| {
                format!(
                    "{}|{}",
                    tool.tool_name,
                    tool.output_sha256.as_deref().unwrap_or("")
                )
            })
        }
        let mut run: Vec<&ToolTrajectory> = Vec::new();
        let mut runs: Vec<Vec<&ToolTrajectory>> = Vec::new();
        for item in &items {
            let TrajectoryItem::Tool(tool) = item else {
                continue;
            };
            let matches_run = match (run.last(), error_signature(tool)) {
                (Some(first), Some(sig)) => error_signature(first) == Some(sig),
                _ => false,
            };
            if matches_run {
                run.push(tool);
            } else {
                if run.len() >= ERROR_LOOP_MIN_RUN {
                    runs.push(std::mem::take(&mut run));
                } else {
                    run.clear();
                }
                if error_signature(tool).is_some() {
                    run.push(tool);
                }
            }
        }
        if run.len() >= ERROR_LOOP_MIN_RUN {
            runs.push(run);
        }
        for run in runs {
            let first = run[0];
            anomalies.push(DiagnosticAnomaly {
                kind: AnomalyKind::ErrorLoop,
                summary: format!(
                    "Error loop: '{}' failed {} times in a row with the same error",
                    first.tool_name,
                    run.len()
                ),
                description: format!(
                    "Tool '{}' failed {} consecutive times with identical output. Retrying verbatim will not help.",
                    first.tool_name,
                    run.len()
                ),
                related_refs: run
                    .iter()
                    .map(|tool| TrajectoryRef {
                        seq: tool.started_seq,
                        entry_id: None,
                        run_id: None,
                        lane: "main".into(),
                    })
                    .collect(),
            });
        }
    }

    // 4. Detect ping-pong cycles (mirrors the runtime tripwire): alternating
    // A→B→A… tool sequences with stable per-position outputs.
    {
        let tools: Vec<&ToolTrajectory> = items
            .iter()
            .filter_map(|item| match item {
                TrajectoryItem::Tool(tool) => Some(tool),
                _ => None,
            })
            .collect();
        // Tool identity for cycle purposes: name + args + output hash.
        let key = |tool: &ToolTrajectory| {
            format!(
                "{}|{}|{}",
                tool.tool_name,
                tool.effective_args,
                tool.output_sha256.as_deref().unwrap_or("")
            )
        };
        for period in [2usize, 3] {
            let mut start = 0;
            while start + period * PING_PONG_MIN_ROUNDS <= tools.len() {
                let block: Vec<String> =
                    tools[start..start + period].iter().map(|t| key(t)).collect();
                let mut rounds = 1;
                while start + (rounds + 1) * period <= tools.len()
                    && tools[start + rounds * period..start + (rounds + 1) * period]
                        .iter()
                        .map(|t| key(t))
                        .collect::<Vec<_>>()
                        == block
                {
                    rounds += 1;
                }
                if rounds >= PING_PONG_MIN_ROUNDS {
                    let names: Vec<String> = tools[start..start + period]
                        .iter()
                        .map(|tool| tool.tool_name.clone())
                        .collect();
                    anomalies.push(DiagnosticAnomaly {
                        kind: AnomalyKind::PingPongCycle,
                        summary: format!(
                            "Ping-pong loop: {} cycled {rounds} times",
                            names.join(" → ")
                        ),
                        description: format!(
                            "Tools {} repeated {rounds} consecutive rounds with identical outputs. The loop is not converging.",
                            names.join(", ")
                        ),
                        related_refs: tools[start..start + rounds * period]
                            .iter()
                            .map(|tool| TrajectoryRef {
                                seq: tool.started_seq,
                                entry_id: None,
                                run_id: None,
                                lane: "main".into(),
                            })
                            .collect(),
                    });
                    start += rounds * period;
                } else {
                    start += 1;
                }
            }
        }
    }

    // 5. Detect provider retry loops: consecutive provider attempts that all
    // errored. Same provider+model+error code required consecutively.
    {
        let mut run: Vec<(&ProviderTrajectory, String)> = Vec::new();
        let mut runs: Vec<Vec<(&ProviderTrajectory, String)>> = Vec::new();
        for item in &items {
            let TrajectoryItem::Provider(provider) = item else {
                continue;
            };
            let signature = provider.error.as_ref().map(|error| {
                format!(
                    "{}|{}|{}",
                    provider.provider,
                    provider.model,
                    error.code.as_ref().map(|code| code.as_str()).unwrap_or_default()
                )
            });
            let matches_run = match (run.last(), &signature) {
                (Some((_, previous)), Some(current)) => previous == current,
                _ => false,
            };
            if matches_run {
                run.push((provider, signature.unwrap_or_default()));
            } else {
                if run.len() >= ERROR_LOOP_MIN_RUN {
                    runs.push(std::mem::take(&mut run));
                } else {
                    run.clear();
                }
                if let Some(current) = signature {
                    run.push((provider, current));
                }
            }
        }
        if run.len() >= ERROR_LOOP_MIN_RUN {
            runs.push(run);
        }
        for run in runs {
            let first = run[0].0;
            anomalies.push(DiagnosticAnomaly {
                kind: AnomalyKind::ProviderRetryLoop,
                summary: format!(
                    "Provider retry loop: {} failed {} times in a row",
                    first.model,
                    run.len()
                ),
                description: format!(
                    "Provider {} ({}) errored {} consecutive times ({}).",
                    first.provider,
                    first.model,
                    run.len(),
                    run[0].1
                ),
                related_refs: run
                    .iter()
                    .map(|(provider, _)| TrajectoryRef {
                        seq: provider.started_seq,
                        entry_id: None,
                        run_id: None,
                        lane: "main".into(),
                    })
                    .collect(),
            });
        }
    }

    SessionTrajectory {
        requests,
        items,
        anomalies,
        total_usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{
        ContextItemSource, ContextItemStatus, ContextManifestItem, JsonlStore, Record,
        ToolExecutionOutcome, ToolExecutionPhase, ToolReplaySafety, TraceString,
    };

    #[test]
    fn test_tool_lifecycle_join() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();

        // 0. OperationStarted
        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 50,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();

        // 0.1 Assistant Entry containing tool call
        store
            .append_entry(Entry::new(
                "ast-1",
                None,
                "main",
                store.next_sequence(),
                75,
                AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_protocol::RuntimeToolCall {
                        id: "call-123".into(),
                        r#type: "function".into(),
                        function: threadlane_protocol::RuntimeToolCallFunction {
                            name: "run_command".into(),
                            arguments: "{\"command\": \"cargo test\"}".into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                false,
            ))
            .unwrap();

        // 1. ToolStarted
        store
            .append_record(Record::ToolStarted {
                id: "tool-start-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 100,
                run_id: "run-1".into(),
                assistant_entry_id: "ast-1".into(),
                tool_index: 0,
                tool_call_id: "call-123".into(),
                tool_name: "run_command".into(),
                effective_args: serde_json::json!({ "command": "cargo test" }),
                result_entry_id: "res-1".into(),
                replay: ToolReplaySafety::Never,
            })
            .unwrap();

        // 2. ToolExecutionObserved (Finished)
        store
            .append_record(Record::ToolExecutionObserved {
                id: "tool-exec-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 200,
                run_id: "run-1".into(),
                attempt: Some(1),
                tool_call_id: TraceString::new("call-123").unwrap(),
                tool_name: TraceString::new("run_command").unwrap(),
                executor_kind: TraceString::new("builtin").unwrap(),
                phase: ToolExecutionPhase::Finished,
                started_at_ms: Some(100),
                duration_ms: Some(150),
                outcome: Some(ToolExecutionOutcome::Succeeded),
                exit_code: Some(0),
                cancelled: false,
                is_error: Some(false),
                terminate: Some(false),
                output_sha256: Some(TraceString::new("output_hash").unwrap()),
                output_bytes: Some(1024),
            })
            .unwrap();

        let traj = project_trajectory(&store);
        let tool_items: Vec<_> = traj
            .items
            .iter()
            .filter_map(|i| match i {
                TrajectoryItem::Tool(t) => Some(t),
                _ => None,
            })
            .collect();

        assert_eq!(tool_items.len(), 1);
        let t = tool_items[0];
        assert_eq!(t.call_id, "call-123");
        assert_eq!(t.tool_name, "run_command");
        assert_eq!(t.status, ToolStatus::Succeeded);
        assert_eq!(t.duration_ms, Some(150));
        assert_eq!(t.exit_code, Some(0));
        assert_eq!(t.output_bytes, Some(1024));
    }

    #[test]
    fn test_request_aggregation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();

        // User input entry
        store
            .append_entry(Entry::new(
                "msg-1",
                None,
                "main",
                1,
                100,
                AgentMessage::User {
                    content: "Hello assistant".into(),
                },
                false,
            ))
            .unwrap();

        // OperationStarted
        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 100,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();

        // Step attempt
        store
            .append_record(Record::StepAttempt {
                id: "step-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 100,
                run_id: "run-1".into(),
                attempt: 1,
                result_entry_id: "res-1".into(),
                compaction_reason: None,
            })
            .unwrap();

        // Assistant entry with tool call
        store
            .append_entry(Entry::new(
                "ast-1",
                Some("msg-1".into()),
                "main",
                store.next_sequence(),
                120,
                AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_protocol::RuntimeToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: threadlane_protocol::RuntimeToolCallFunction {
                            name: "write_file".into(),
                            arguments: "{\"path\": \"src/lib.rs\"}".into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                false,
            ))
            .unwrap();

        // Tool start modifying a file
        store
            .append_record(Record::ToolStarted {
                id: "tool-start-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 150,
                run_id: "run-1".into(),
                assistant_entry_id: "ast-1".into(),
                tool_index: 0,
                tool_call_id: "call-1".into(),
                tool_name: "write_file".into(),
                effective_args: serde_json::json!({ "path": "src/lib.rs" }),
                result_entry_id: "res-tool-1".into(),
                replay: ToolReplaySafety::Never,
            })
            .unwrap();

        // Assistant final answer
        store
            .append_entry(Entry::new(
                "msg-2",
                Some("ast-1".into()),
                "main",
                store.next_sequence(),
                200,
                AgentMessage::Assistant {
                    content: Some("I updated src/lib.rs".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
                false,
            ))
            .unwrap();

        let traj = project_trajectory(&store);
        assert_eq!(traj.requests.len(), 1);
        let req = &traj.requests[0];
        assert_eq!(req.request_index, 1);
        assert_eq!(req.prompt_text, "Hello assistant");
        assert_eq!(req.turn_count, 1);
        assert_eq!(req.tool_calls_count, 1);
        assert_eq!(req.files_mutated, vec!["src/lib.rs".to_string()]);
        assert_eq!(
            req.final_assistant_text.as_deref(),
            Some("I updated src/lib.rs")
        );
    }

    #[test]
    fn test_context_manifest_projection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();

        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 50,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();

        // Current canonical order: request start is durable before its manifest.
        store
            .append_record(Record::ProviderRequestStarted {
                id: "provider-start-canonical".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 75,
                run_id: "run-1".into(),
                attempt: 1,
                provider: TraceString::new("test").unwrap(),
                model: TraceString::new("model").unwrap(),
                request_id: Some(TraceString::new("req-123").unwrap()),
            })
            .unwrap();
        let items = vec![ContextManifestItem {
            position: 0,
            source: ContextItemSource::SystemPrompt,
            entry_id: None,
            role: TraceString::new("system").unwrap(),
            token_estimate: 50,
            status: ContextItemStatus::Active,
            digest_sha256: TraceString::new("sha-system").unwrap(),
            label: None,
        }];

        store
            .append_record(Record::ContextManifestCaptured {
                id: "context-manifest-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 100,
                run_id: "run-1".into(),
                attempt: 1,
                request_id: TraceString::new("req-123").unwrap(),
                total_estimated_tokens: Some(50),
                effective_model: None,
                context_limit: None,
                context_limit_is_estimate: false,
                compaction_generation: 0,
                items,
            })
            .unwrap();

        // Legacy order remains joinable as well.
        store
            .append_record(Record::ContextManifestCaptured {
                id: "context-manifest-legacy".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 110,
                run_id: "run-1".into(),
                attempt: 2,
                request_id: TraceString::new("req-legacy").unwrap(),
                total_estimated_tokens: Some(1),
                effective_model: None,
                context_limit: None,
                context_limit_is_estimate: false,
                compaction_generation: 0,
                items: Vec::new(),
            })
            .unwrap();
        store
            .append_record(Record::ProviderRequestStarted {
                id: "provider-start-legacy".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 120,
                run_id: "run-1".into(),
                attempt: 2,
                provider: TraceString::new("test").unwrap(),
                model: TraceString::new("model").unwrap(),
                request_id: Some(TraceString::new("req-legacy").unwrap()),
            })
            .unwrap();

        let traj = project_trajectory(&store);
        let manifests: Vec<_> = traj
            .items
            .iter()
            .filter_map(|i| match i {
                TrajectoryItem::ContextManifest(m) => Some(m),
                _ => None,
            })
            .collect();

        assert_eq!(manifests.len(), 2);
        let canonical = manifests
            .iter()
            .find(|manifest| manifest.request_id == "req-123")
            .unwrap();
        assert_eq!(canonical.total_estimated_tokens, Some(50));
        assert_eq!(canonical.items.len(), 1);
        let providers: Vec<_> = traj
            .items
            .iter()
            .filter_map(|item| match item {
                TrajectoryItem::Provider(provider) => Some(provider),
                _ => None,
            })
            .collect();
        assert_eq!(providers.len(), 2);
        for provider in providers {
            let manifest = provider
                .context_manifest_ref
                .as_ref()
                .expect("manifest join");
            let expected = manifests
                .iter()
                .find(|candidate| candidate.request_id == provider.request_id)
                .unwrap();
            assert_eq!(manifest.seq, expected.seq);
        }
    }

    #[test]
    fn context_snapshot_trajectory_reports_captures_repeats_and_unknown_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        let digest_a = TraceString::new("a".repeat(64)).unwrap();
        let digest_b = TraceString::new("b".repeat(64)).unwrap();

        for (context_id, tool_call_id, digest) in [
            ("ctx-z", "call-z", digest_a.clone()),
            ("ctx-b", "call-b", digest_b.clone()),
            ("ctx-a", "call-a", digest_a.clone()),
        ] {
            let entry_id = format!("result-{context_id}");
            store
                .append_entry(Entry::new(
                    entry_id.clone(),
                    None,
                    "main",
                    store.next_sequence(),
                    1,
                    AgentMessage::Tool {
                        tool_call_id: tool_call_id.into(),
                        name: "read_file".into(),
                        content: "private snapshot body".into(),
                        is_error: false,
                        terminate: false,
                        images: Vec::new(),
                    },
                    false,
                ))
                .unwrap();
            store
                .append_record(Record::ContextSnapshotIndexed {
                    id: format!("index-{context_id}"),
                    seq: store.next_sequence(),
                    lane: "main".into(),
                    timestamp: 1,
                    run_id: "run-1".into(),
                    snapshot: crate::harness::ContextSnapshot {
                        context_id: context_id.into(),
                        source_lane: "main".into(),
                        source_run_id: "run-1".into(),
                        source_tool_call_id: tool_call_id.into(),
                        source_entry_id: entry_id,
                        path: "src/lib.rs".into(),
                        start_line: Some(10),
                        end_line: Some(20),
                        file_sha256: digest,
                        output_chars: 21,
                        captured_at: 1,
                    },
                })
                .unwrap();
        }

        for (context_id, digest, lane) in [
            ("ctx-a", digest_a.clone(), "child"),
            ("ctx-z", digest_a, "main"),
            ("ctx-b", digest_b, "other-child"),
        ] {
            let seq = store.next_sequence();
            store
                .append_record(Record::ContextSnapshotLoaded {
                    id: format!("load-{context_id}"),
                    seq,
                    lane: lane.into(),
                    timestamp: seq,
                    run_id: "run-1".into(),
                    context_id: context_id.into(),
                    source_lane: "main".into(),
                    current_digest: Some(digest),
                    outcome: crate::harness::ContextSnapshotLoadOutcome::Loaded,
                })
                .unwrap();
        }
        let seq = store.next_sequence();
        store
            .append_record(Record::ContextSnapshotLoaded {
                id: "load-ctx-unknown".into(),
                seq,
                lane: "child".into(),
                timestamp: seq,
                run_id: "run-1".into(),
                context_id: "ctx-unknown".into(),
                source_lane: "main".into(),
                current_digest: None,
                outcome: crate::harness::ContextSnapshotLoadOutcome::Missing,
            })
            .unwrap();

        let trajectory = project_trajectory(&store);
        let captures: Vec<_> = trajectory
            .items
            .iter()
            .filter_map(|item| match item {
                TrajectoryItem::ContextSnapshotCapture(capture) => Some(capture),
                _ => None,
            })
            .collect();
        assert_eq!(
            captures
                .iter()
                .map(|capture| capture.context_id.as_str())
                .collect::<Vec<_>>(),
            vec!["ctx-z", "ctx-b", "ctx-a"]
        );
        assert_eq!(
            captures
                .iter()
                .map(|capture| capture.duplicate_candidate)
                .collect::<Vec<_>>(),
            vec![false, false, true]
        );

        let loads: Vec<_> = trajectory
            .items
            .iter()
            .filter_map(|item| match item {
                TrajectoryItem::ContextSnapshotLoad(load) => Some(load),
                _ => None,
            })
            .collect();

        assert_eq!(
            loads
                .iter()
                .map(|load| load.context_id.as_str())
                .collect::<Vec<_>>(),
            vec!["ctx-a", "ctx-z", "ctx-b", "ctx-unknown"]
        );
        assert_eq!(
            loads
                .iter()
                .map(|load| load.duplicate_candidate)
                .collect::<Vec<_>>(),
            vec![true, false, false, false]
        );
        assert!(loads[..3]
            .iter()
            .all(|load| load.path.as_deref() == Some("src/lib.rs")));
        assert_eq!(loads[3].path, None);
    }

    #[test]
    fn test_anomaly_detection_repeated_tools() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();

        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 50,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();

        let tool_calls = (1..=4)
            .map(|i| threadlane_protocol::RuntimeToolCall {
                id: format!("call-{i}"),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "read_file".into(),
                    arguments: "{\"path\": \"src/main.rs\"}".into(),
                },
                thought_signature: None,
            })
            .collect();

        store
            .append_entry(Entry::new(
                "ast-1",
                None,
                "main",
                store.next_sequence(),
                60,
                AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(tool_calls),
                    stop_reason: None,
                    deferred_handle: None,
                },
                false,
            ))
            .unwrap();

        for i in 1..=4 {
            store
                .append_record(Record::ToolStarted {
                    id: format!("tool-start-{i}"),
                    seq: store.next_sequence(),
                    lane: "main".into(),
                    timestamp: 100 * i,
                    run_id: "run-1".into(),
                    assistant_entry_id: "ast-1".into(),
                    tool_index: (i - 1) as usize,
                    tool_call_id: format!("call-{i}"),
                    tool_name: "read_file".into(),
                    effective_args: serde_json::json!({ "path": "src/main.rs" }),
                    result_entry_id: format!("res-{i}"),
                    replay: ToolReplaySafety::Safe,
                })
                .unwrap();
        }

        let traj = project_trajectory(&store);
        let repeat_anomaly = traj
            .anomalies
            .iter()
            .find(|a| a.kind == AnomalyKind::RepeatedToolCallIdenticalArgs)
            .expect("should find repeated tool call anomaly");
        assert_eq!(repeat_anomaly.related_refs.len(), 4);

        let orphaned_count = traj
            .anomalies
            .iter()
            .filter(|a| a.kind == AnomalyKind::OrphanedToolStart)
            .count();
        assert_eq!(orphaned_count, 4);
    }

    fn append_finished_tool(
        store: &mut JsonlStore,
        index: usize,
        name: &str,
        args: serde_json::Value,
        failed: bool,
        output_hash: &str,
    ) {
        use crate::harness::{ToolExecutionOutcome, ToolExecutionPhase};
        let call_id = format!("call-{index}");
        store
            .append_entry(Entry::new(
                format!("ast-{index}"),
                None,
                "main",
                store.next_sequence(),
                60,
                AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_protocol::RuntimeToolCall {
                        id: call_id.clone(),
                        r#type: "function".into(),
                        function: threadlane_protocol::RuntimeToolCallFunction {
                            name: name.into(),
                            arguments: args.to_string(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                false,
            ))
            .unwrap();
        store
            .append_record(Record::ToolStarted {
                id: format!("tool-start-{index}"),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 100 * index as u64,
                run_id: "run-1".into(),
                assistant_entry_id: format!("ast-{index}"),
                tool_index: 0,
                tool_call_id: call_id.clone(),
                tool_name: name.into(),
                effective_args: args,
                result_entry_id: format!("res-{index}"),
                replay: ToolReplaySafety::Never,
            })
            .unwrap();
        store
            .append_record(Record::ToolExecutionObserved {
                id: format!("tool-exec-{index}"),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 100 * index as u64 + 1,
                run_id: "run-1".into(),
                attempt: Some(1),
                tool_call_id: TraceString::new(call_id).unwrap(),
                tool_name: TraceString::new(name).unwrap(),
                executor_kind: TraceString::new("builtin").unwrap(),
                phase: ToolExecutionPhase::Finished,
                started_at_ms: Some(100),
                duration_ms: Some(10),
                outcome: Some(if failed {
                    ToolExecutionOutcome::Failed
                } else {
                    ToolExecutionOutcome::Succeeded
                }),
                exit_code: Some(if failed { 1 } else { 0 }),
                cancelled: false,
                is_error: Some(failed),
                terminate: Some(false),
                output_sha256: Some(TraceString::new(output_hash).unwrap()),
                output_bytes: Some(64),
            })
            .unwrap();
    }

    #[test]
    fn error_loop_anomaly_needs_consecutive_same_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 50,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();
        // Two failures, a success, then three identical failures: only the
        // trailing run trips.
        for (index, (args, failed, hash)) in [
            (serde_json::json!({"path": "a"}), true, "h1"),
            (serde_json::json!({"path": "b"}), true, "h1"),
            (serde_json::json!({"path": "c"}), false, "h1"),
            (serde_json::json!({"path": "d"}), true, "h9"),
            (serde_json::json!({"path": "e"}), true, "h9"),
            (serde_json::json!({"path": "f"}), true, "h9"),
        ]
        .into_iter()
        .enumerate()
        {
            append_finished_tool(&mut store, index + 1, "read_file", args, failed, hash);
        }
        let traj = project_trajectory(&store);
        let loops: Vec<_> = traj
            .anomalies
            .iter()
            .filter(|a| a.kind == AnomalyKind::ErrorLoop)
            .collect();
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].related_refs.len(), 3);
        assert!(loops[0].summary.contains("3 times"));
    }

    #[test]
    fn ping_pong_anomaly_needs_stable_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 50,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();
        // read/list alternating with stable outputs: 3 rounds trip.
        for round in 0..3 {
            append_finished_tool(
                &mut store,
                round * 2 + 1,
                "read_file",
                serde_json::json!({}),
                false,
                "ha",
            );
            append_finished_tool(
                &mut store,
                round * 2 + 2,
                "list_dir",
                serde_json::json!({}),
                false,
                "hb",
            );
        }
        let traj = project_trajectory(&store);
        let cycles: Vec<_> = traj
            .anomalies
            .iter()
            .filter(|a| a.kind == AnomalyKind::PingPongCycle)
            .collect();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].related_refs.len(), 6);

        // Evolving outputs never trip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 50,
                source_leaf_id: None,
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();
        for round in 0..4 {
            append_finished_tool(
                &mut store,
                round * 2 + 1,
                "read_file",
                serde_json::json!({}),
                false,
                &format!("ha{round}"),
            );
            append_finished_tool(
                &mut store,
                round * 2 + 2,
                "list_dir",
                serde_json::json!({}),
                false,
                &format!("hb{round}"),
            );
        }
        let traj = project_trajectory(&store);
        assert!(traj
            .anomalies
            .iter()
            .all(|a| a.kind != AnomalyKind::PingPongCycle));
    }

    #[test]
    fn provider_retry_loop_needs_consecutive_same_errors() {
        use crate::harness::{ErrorCategory, ProviderErrorSummary, ProviderOutcome};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        // Attempts 1-2 fail with 429 (a run of 2: silent), attempt 3
        // succeeds (breaks the run), attempts 4-6 fail with 429 (trips).
        for attempt in 1..=6u32 {
            let (outcome, error) = if attempt == 3 {
                (ProviderOutcome::Completed, None)
            } else {
                (
                    ProviderOutcome::Failed,
                    Some(ProviderErrorSummary {
                        category: ErrorCategory::RateLimit,
                        code: TraceString::new("429").ok(),
                        retryable: true,
                    }),
                )
            };
            let request_id = format!("req-{attempt}");
            store
                .append_record(Record::ProviderRequestStarted {
                    id: format!("provider-start-{attempt}"),
                    seq: store.next_sequence(),
                    lane: "main".into(),
                    timestamp: 10 * attempt as u64,
                    run_id: "run-1".into(),
                    attempt,
                    provider: TraceString::new("test").unwrap(),
                    model: TraceString::new("model").unwrap(),
                    request_id: Some(TraceString::new(request_id.clone()).unwrap()),
                })
                .unwrap();
            store
                .append_record(Record::ProviderRequestFinished {
                    id: format!("provider-finish-{attempt}"),
                    seq: store.next_sequence(),
                    lane: "main".into(),
                    timestamp: 10 * attempt as u64 + 1,
                    run_id: "run-1".into(),
                    attempt,
                    request_id: Some(TraceString::new(request_id).unwrap()),
                    outcome,
                    error,
                    duration_ms: Some(5),
                    usage: None,
                })
                .unwrap();
        }
        let traj = project_trajectory(&store);
        let loops: Vec<_> = traj
            .anomalies
            .iter()
            .filter(|a| a.kind == AnomalyKind::ProviderRetryLoop)
            .collect();
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].related_refs.len(), 3);
        assert!(loops[0].summary.contains("3 times"));
    }
}
