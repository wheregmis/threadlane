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
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub lane: String,
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
    pub request_index: u32,
    pub prompt_entry_id: String,
    pub prompt_text: String,
    pub started_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub status: RequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_assistant_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_assistant_text: Option<String>,
    pub turn_count: u32,
    pub tool_calls_count: u32,
    pub files_mutated: Vec<String>,
    pub commands_executed: Vec<String>,
    pub usage: TokenUsage,
    pub root_ref: TrajectoryRef,
    pub item_refs: Vec<TrajectoryRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextManifestTrajectory {
    pub request_id: String,
    pub attempt: u32,
    pub seq: u64,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_estimated_tokens: Option<u32>,
    pub items: Vec<ContextManifestItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_ref: Option<TrajectoryRef>,
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
    pub attempt: u32,
    pub request_id: String,
    pub provider: String,
    pub model: String,
    pub started_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ProviderOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProviderErrorSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_manifest_ref: Option<TrajectoryRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_entry: Option<TrajectoryRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_entry_id: Option<String>,
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
    pub call_id: String,
    pub tool_name: String,
    pub effective_args: serde_json::Value,
    pub status: ToolStatus,
    pub started_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_content: Option<String>,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_assistant_entry: Option<TrajectoryRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_entry: Option<TrajectoryRef>,
    pub raw_record_seqs: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionTrajectory {
    pub request_id: String,
    pub capability: String,
    pub scopes: Vec<PermissionTraceScope>,
    pub requested_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PermissionTraceDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub source: PermissionTraceSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentTrajectory {
    pub child_run_id: String,
    pub agent_id: String,
    pub subagent_lane: String,
    pub phase: SubagentLifecyclePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenericDurableTrajectory {
    pub seq: u64,
    pub id: String,
    pub lane: String,
    pub category: String,
    pub summary: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnomalyKind {
    RepeatedToolCallIdenticalArgs,
    ProviderRetryLoop,
    OrphanedToolStart,
    ContextOverflowRisk,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticAnomaly {
    pub kind: AnomalyKind,
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
    pub fn seq(&self) -> u64 {
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
    pub requests: Vec<RequestTrajectory>,
    pub items: Vec<TrajectoryItem>,
    pub anomalies: Vec<DiagnosticAnomaly>,
    pub total_usage: TokenUsage,
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

    // Collect all chronological journal events (entries & records sorted by seq)
    #[derive(Clone)]
    enum JournalItem<'a> {
        Entry(&'a Entry),
        Record(&'a Record),
    }
    let mut journal: Vec<JournalItem> = Vec::new();
    for entry in store.entries() {
        journal.push(JournalItem::Entry(entry));
    }
    for record in store.records() {
        journal.push(JournalItem::Record(record));
    }
    journal.sort_by_key(|item| match item {
        JournalItem::Entry(e) => e.seq,
        JournalItem::Record(r) => r.seq(),
    });

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
                                if !req.files_mutated.contains(&mutated) {
                                    req.files_mutated.push(mutated);
                                }
                            }
                            if let Some(cmd) = extract_command(tool_name, effective_args) {
                                if !req.commands_executed.contains(&cmd) {
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
        ToolReplaySafety, TraceString,
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
}
