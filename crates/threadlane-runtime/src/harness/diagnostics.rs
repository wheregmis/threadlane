use super::{LaneStatus, QueueKind, ReducedState, SessionStore, ToolReplaySafety};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDecision {
    None,
    ResumeFromLeaf,
    ReplaySafeToolsThenResume,
    AbortUnsafeTool,
    WaitForDeferredResult,
    ExplicitRetryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedToolDiagnostic {
    pub run_id: String,
    pub call_id: String,
    pub name: String,
    pub result_entry_id: String,
    pub replay: ToolReplaySafety,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedWorkDiagnostic {
    pub entry_id: String,
    pub queue: QueueKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryPlan {
    pub(crate) session_id: String,
    pub(crate) lane: String,
    pub(crate) source_sequence: u64,
    pub(crate) decision: RecoveryDecision,
    pub(crate) open_operation: Option<String>,
    pub(crate) interrupted_tools: Vec<InterruptedToolDiagnostic>,
    pub(crate) queued_work: Vec<QueuedWorkDiagnostic>,
    pub(crate) open_operation_ids: Vec<String>,
    pub(crate) safe_tools_to_replay: Vec<crate::Record>,
    pub(crate) unreplayable_tools: usize,
    pub(crate) abort_requested_operation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneRecoveryDiagnostic {
    pub lane: String,
    pub status: LaneStatus,
    pub leaf_id: Option<String>,
    pub open_operation: Option<String>,
    pub attempts: u32,
    pub abort_requested: bool,
    pub decision: RecoveryDecision,
    pub interrupted_tools: Vec<InterruptedToolDiagnostic>,
    pub queued_work: Vec<QueuedWorkDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelContextDiagnostic {
    pub seq: u64,
    pub id: String,
    pub lane: String,
    role: String,
    pub message: crate::types::AgentMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableEventDiagnostic {
    pub seq: u64,
    pub id: String,
    pub lane: String,
    pub run_id: Option<String>,
    pub turn: Option<u32>,
    pub kind: DurableEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableEventKind {
    Entry {
        role: String,
        parent_id: Option<String>,
    },
    Record,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionDiagnostics {
    pub model_context: Vec<ModelContextDiagnostic>,
    pub durable_events: Vec<DurableEventDiagnostic>,
    pub recovery: Vec<LaneRecoveryDiagnostic>,
}

pub fn project_session_diagnostics<S: SessionStore>(
    store: &S,
    lane: &str,
) -> Result<SessionDiagnostics, super::ReduceError> {
    let model_context = store
        .model_context(lane)
        .map_err(|error| super::ReduceError::Storage(error.to_string()))?
        .entries
        .into_iter()
        .map(|entry| ModelContextDiagnostic {
            seq: entry.seq,
            id: entry.id,
            lane: entry.lane,
            role: entry.message.role_str().to_owned(),
            message: entry.message,
        })
        .collect();
    let mut durable_events = store
        .entries()
        .iter()
        .map(|entry| DurableEventDiagnostic {
            seq: entry.seq,
            id: entry.id.clone(),
            lane: entry.lane.clone(),
            run_id: None,
            turn: None,
            kind: DurableEventKind::Entry {
                role: entry.message.role_str().to_owned(),
                parent_id: entry.parent_id.clone(),
            },
        })
        .collect::<Vec<_>>();
    durable_events.extend(store.records().iter().map(|record| DurableEventDiagnostic {
        seq: record.seq(),
        id: record.id().to_owned(),
        lane: record.lane().to_owned(),
        run_id: record.run_id().map(str::to_owned),
        turn: record.turn(),
        kind: DurableEventKind::Record,
    }));
    durable_events.sort_by_key(|event| event.seq);
    let reduced = super::Reducer::reduce(store)?;
    Ok(SessionDiagnostics {
        model_context,
        durable_events,
        recovery: project_recovery(&reduced),
    })
}

pub fn project_recovery(state: &ReducedState) -> Vec<LaneRecoveryDiagnostic> {
    state
        .lanes
        .iter()
        .map(|lane| {
            let interrupted_tools = lane
                .tools
                .iter()
                .filter(|tool| !tool.completed)
                .map(|tool| InterruptedToolDiagnostic {
                    run_id: tool.run_id.clone(),
                    call_id: tool.tool_call_id.clone(),
                    name: tool.tool_name.clone(),
                    result_entry_id: tool.result_entry_id.clone(),
                    replay: tool.replay.clone(),
                })
                .collect::<Vec<_>>();
            let has_unsafe = interrupted_tools
                .iter()
                .any(|tool| tool.replay == ToolReplaySafety::Never);
            let has_safe = interrupted_tools
                .iter()
                .any(|tool| tool.replay == ToolReplaySafety::Safe);
            let decision = match lane.status {
                LaneStatus::SuspendedCrash if has_unsafe => RecoveryDecision::AbortUnsafeTool,
                LaneStatus::SuspendedCrash if has_safe => {
                    RecoveryDecision::ReplaySafeToolsThenResume
                }
                LaneStatus::SuspendedCrash => RecoveryDecision::ResumeFromLeaf,
                LaneStatus::SuspendedDeferred => RecoveryDecision::WaitForDeferredResult,
                LaneStatus::Failed => RecoveryDecision::ExplicitRetryRequired,
                LaneStatus::Idle | LaneStatus::Completed => RecoveryDecision::None,
            };
            LaneRecoveryDiagnostic {
                lane: lane.name.clone(),
                status: lane.status.clone(),
                leaf_id: lane.leaf_id.clone(),
                open_operation: lane.open_operation.clone(),
                attempts: lane.attempts,
                abort_requested: lane.abort_requested,
                decision,
                interrupted_tools,
                queued_work: lane
                    .queued
                    .iter()
                    .map(|queued| QueuedWorkDiagnostic {
                        entry_id: queued.target.id.clone(),
                        queue: queued.queue.clone(),
                    })
                    .collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{LaneState, QueuedEntry, SteerPriority, ToolState};
    use crate::types::{AgentMessage, TokenUsage};
    use std::collections::BTreeMap;

    fn lane(status: LaneStatus) -> LaneState {
        LaneState {
            name: "main".into(),
            status,
            leaf_id: Some("leaf".into()),
            open_operation: Some("run".into()),
            attempts: 2,
            retry: None,
            queued: Vec::new(),
            deferred_writes: Vec::new(),
            abort_requested: false,
            usage: TokenUsage::default(),
            tools: Vec::new(),
            context_snapshots: Vec::new(),
            facts: BTreeMap::new(),
            resume_data: BTreeMap::new(),
        }
    }

    fn interrupted(replay: ToolReplaySafety) -> ToolState {
        ToolState {
            run_id: "run".into(),
            assistant_entry_id: "assistant".into(),
            tool_index: 0,
            tool_call_id: "call".into(),
            tool_name: "read_file".into(),
            result_entry_id: "result".into(),
            replay,
            completed: false,
            terminate: false,
        }
    }

    #[test]
    fn recovery_decisions_are_canonical_for_lane_state() {
        let statuses = [
            (LaneStatus::Idle, RecoveryDecision::None),
            (LaneStatus::Completed, RecoveryDecision::None),
            (LaneStatus::Failed, RecoveryDecision::ExplicitRetryRequired),
            (
                LaneStatus::SuspendedDeferred,
                RecoveryDecision::WaitForDeferredResult,
            ),
            (LaneStatus::SuspendedCrash, RecoveryDecision::ResumeFromLeaf),
        ];
        for (status, expected) in statuses {
            let diagnostic = project_recovery(&ReducedState {
                lanes: vec![lane(status)],
            });
            assert_eq!(diagnostic[0].decision, expected);
        }
    }

    #[test]
    fn unsafe_interruption_wins_over_safe_replay() {
        let mut lane = lane(LaneStatus::SuspendedCrash);
        lane.tools = vec![
            interrupted(ToolReplaySafety::Safe),
            interrupted(ToolReplaySafety::Never),
        ];
        let diagnostic = project_recovery(&ReducedState { lanes: vec![lane] });
        assert_eq!(diagnostic[0].decision, RecoveryDecision::AbortUnsafeTool);
        assert_eq!(diagnostic[0].interrupted_tools.len(), 2);
    }

    #[test]
    fn queued_work_is_projected_without_message_payload() {
        let mut lane = lane(LaneStatus::Idle);
        lane.queued.push(QueuedEntry {
            id: "queue-record".into(),
            run_id: None,
            queue: QueueKind::FollowUp,
            priority: Some(SteerPriority::Normal),
            target: crate::harness::ProvisionedEntry {
                id: "queued".into(),
                message: AgentMessage::User {
                    content: "secret payload".into(),
                },
                parent_id: None,
                surface_op: crate::harness::SurfaceOperation::Append,
            },
        });
        let diagnostic = project_recovery(&ReducedState { lanes: vec![lane] });
        assert_eq!(diagnostic[0].queued_work[0].entry_id, "queued");
        assert_eq!(diagnostic[0].queued_work[0].queue, QueueKind::FollowUp);
    }
}
