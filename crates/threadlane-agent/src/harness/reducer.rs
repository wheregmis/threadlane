use super::store::SessionStore;
use super::types::{
    Entry, LaneState, LaneStatus, OperationOutcome, Record, ReduceError, ReducedState,
};
use std::collections::HashMap;

pub struct Reducer;

pub(crate) fn validate_candidate_entry<S: SessionStore>(
    store: &S,
    entry: &Entry,
) -> Result<(), ReduceError> {
    let mut entries = store.entries().to_vec();
    entries.push(entry.clone());
    validate_candidate(store, entries, store.records().to_vec())
}

pub(crate) fn validate_candidate_record<S: SessionStore>(
    store: &S,
    record: &Record,
) -> Result<(), ReduceError> {
    let mut records = store.records().to_vec();
    records.push(record.clone());
    validate_candidate(store, store.entries().to_vec(), records)
}

fn validate_candidate<S: SessionStore>(
    store: &S,
    entries: Vec<Entry>,
    records: Vec<Record>,
) -> Result<(), ReduceError> {
    struct CandidateStore {
        session_id: String,
        entries: Vec<Entry>,
        records: Vec<Record>,
    }

    impl SessionStore for CandidateStore {
        fn session_id(&self) -> &str {
            &self.session_id
        }
        fn entries(&self) -> &[Entry] {
            &self.entries
        }
        fn records(&self) -> &[Record] {
            &self.records
        }
        fn append_entry(&mut self, _entry: Entry) -> Result<(), ReduceError> {
            Err(ReduceError::Storage("validation view is read-only".into()))
        }
        fn append_record(&mut self, _record: Record) -> Result<(), ReduceError> {
            Err(ReduceError::Storage("validation view is read-only".into()))
        }
    }

    Reducer::reduce(&CandidateStore {
        session_id: store.session_id().to_owned(),
        entries,
        records,
    })
    .map(|_| ())
}

impl Reducer {
    pub fn reduce<S: SessionStore>(store: &S) -> Result<ReducedState, ReduceError> {
        let mut ids = std::collections::HashSet::new();
        for entry in store.entries() {
            if entry.id.trim().is_empty() {
                return Err(ReduceError::InvalidRecord("empty entry id".into()));
            }
            if !ids.insert(entry.id.as_str()) {
                return Err(ReduceError::DuplicateId(entry.id.clone()));
            }
        }
        for record in store.records() {
            if record.id().trim().is_empty() {
                return Err(ReduceError::InvalidRecord("empty record id".into()));
            }
            if !ids.insert(record.id()) {
                return Err(ReduceError::DuplicateId(record.id().into()));
            }
        }
        let mut sequence = store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(store.records().iter().map(Record::seq))
            .collect::<Vec<_>>();
        sequence.sort_unstable();
        for pair in sequence.windows(2) {
            if pair[0] >= pair[1] {
                return Err(ReduceError::NonMonotonicSequence {
                    previous: pair[0],
                    current: pair[1],
                });
            }
        }
        let mut lanes = HashMap::<String, LaneState>::from([(
            "main".into(),
            LaneState {
                name: "main".into(),
                status: LaneStatus::Idle,
                leaf_id: None,
                open_operation: None,
                attempts: 0,
                retry: None,
                queued: Vec::new(),
                deferred_writes: Vec::new(),
                abort_requested: false,
                usage: Default::default(),
                tools: Vec::new(),
                facts: store.facts(),
                resume_data: Default::default(),
            },
        )]);
        let entry_ids: std::collections::HashSet<_> = store
            .entries()
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        for entry in store.entries() {
            if let Some(parent) = &entry.parent_id {
                if !entry_ids.contains(parent.as_str()) {
                    return Err(ReduceError::MissingParent(parent.clone()));
                }
            }
            if entry.lane.trim().is_empty() {
                return Err(ReduceError::InvalidLane(entry.lane.clone()));
            }
            let lane = lanes
                .entry(entry.lane.clone())
                .or_insert_with(|| LaneState {
                    name: entry.lane.clone(),
                    status: LaneStatus::Idle,
                    leaf_id: None,
                    open_operation: None,
                    attempts: 0,
                    retry: None,
                    queued: Vec::new(),
                    deferred_writes: Vec::new(),
                    abort_requested: false,
                    usage: Default::default(),
                    tools: Vec::new(),
                    facts: Default::default(),
                    resume_data: Default::default(),
                });
            let current_seq = lane.leaf_id.as_deref().and_then(|current_id| {
                store
                    .entries()
                    .iter()
                    .find(|candidate| candidate.id == current_id)
                    .map(|current| current.seq)
            });
            if current_seq.is_none_or(|current_seq| entry.seq >= current_seq) {
                lane.leaf_id = Some(entry.id.clone());
            }
        }
        for lane in lanes.values_mut() {
            let has_v2_operation = store
                .records()
                .iter()
                .any(|record| matches!(record, Record::OperationStarted { .. }));
            if !has_v2_operation {
                if let Some(preferred) = store.preferred_leaf(&lane.name) {
                    if !entry_ids.contains(preferred.as_str()) {
                        return Err(ReduceError::MissingParent(preferred));
                    }
                    lane.leaf_id = Some(preferred);
                }
            }
        }
        for record in store.records() {
            let lane_name = record.lane().to_owned();
            if lane_name.trim().is_empty() {
                return Err(ReduceError::InvalidLane(lane_name));
            }
            let lane = lanes.entry(lane_name.clone()).or_insert_with(|| LaneState {
                name: lane_name.clone(),
                status: LaneStatus::Idle,
                leaf_id: None,
                open_operation: None,
                attempts: 0,
                retry: None,
                queued: Vec::new(),
                deferred_writes: Vec::new(),
                abort_requested: false,
                usage: Default::default(),
                tools: Vec::new(),
                facts: Default::default(),
                resume_data: Default::default(),
            });
            match record {
                Record::OperationStarted {
                    id,
                    seq,
                    source_leaf_id,
                    ..
                } => {
                    if let Some(leaf_id) = source_leaf_id {
                        if !entry_ids.contains(leaf_id.as_str()) {
                            return Err(ReduceError::MissingParent(leaf_id.clone()));
                        }
                        let source_seq = store
                            .entries()
                            .iter()
                            .find(|entry| entry.id == *leaf_id)
                            .map(|entry| entry.seq);
                        let current_seq = lane.leaf_id.as_deref().and_then(|current_id| {
                            store
                                .entries()
                                .iter()
                                .find(|entry| entry.id == current_id)
                                .map(|entry| entry.seq)
                        });
                        if source_seq >= current_seq {
                            lane.leaf_id = Some(leaf_id.clone());
                        }
                    }
                    if let Some(entry) = store
                        .entries()
                        .iter()
                        .filter(|entry| entry.lane == lane.name && entry.seq > *seq)
                        .max_by_key(|entry| entry.seq)
                    {
                        lane.leaf_id = Some(entry.id.clone());
                    }
                    if lane.open_operation.is_some() {
                        return Err(ReduceError::MultipleOpenOperations(lane.name.clone()));
                    }
                    lane.open_operation = Some(id.clone());
                    lane.attempts = 0;
                    lane.retry = None;
                    lane.status = LaneStatus::SuspendedCrash;
                }
                Record::AbortRequested { run_id, .. } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    lane.abort_requested = true;
                }
                Record::OperationFinished {
                    run_id, outcome, ..
                } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    if lane
                        .tools
                        .iter()
                        .any(|tool| tool.run_id == *run_id && !tool.completed)
                    {
                        return Err(ReduceError::InvalidRecord(
                            "operation finished with an incomplete tool batch".into(),
                        ));
                    }
                    if lane.retry.is_some() {
                        return Err(ReduceError::InvalidRecord(
                            "operation finished while a retry is scheduled".into(),
                        ));
                    }
                    lane.open_operation = None;
                    lane.abort_requested = false;
                    lane.retry = None;
                    lane.status = match outcome {
                        OperationOutcome::Completed => LaneStatus::Completed,
                        OperationOutcome::Failed => LaneStatus::Failed,
                        _ => LaneStatus::Idle,
                    };
                }
                Record::LaneMoved {
                    run_id,
                    target_leaf_id,
                    ..
                } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    if !entry_ids.contains(target_leaf_id.as_str()) {
                        return Err(ReduceError::MissingParent(target_leaf_id.clone()));
                    }
                    lane.leaf_id = Some(target_leaf_id.clone());
                }
                Record::QueueEnqueued {
                    id,
                    run_id,
                    queue,
                    priority,
                    target,
                    ..
                } => {
                    if target.id.trim().is_empty() {
                        return Err(ReduceError::InvalidRecord("empty queued entry id".into()));
                    }
                    if let Some(parent_id) = target.parent_id.as_deref() {
                        if !entry_ids.contains(parent_id) {
                            return Err(ReduceError::MissingParent(parent_id.into()));
                        }
                    }
                    if lane
                        .queued
                        .iter()
                        .any(|queued| queued.target.id == target.id)
                    {
                        return Err(ReduceError::InvalidRecord(
                            "queued entry is duplicated".into(),
                        ));
                    }
                    lane.queued.push(super::types::QueuedEntry {
                        id: id.clone(),
                        run_id: run_id.clone(),
                        queue: queue.clone(),
                        priority: *priority,
                        target: target.clone(),
                    });
                }
                Record::QueueCancelled {
                    run_id, entry_id, ..
                } => {
                    let Some(index) = lane.queued.iter().position(|queued| {
                        queued.target.id == *entry_id
                            && (queued.run_id.as_deref() == Some(run_id.as_str())
                                || (queued.run_id.is_none() && run_id.is_empty()))
                    }) else {
                        return Err(ReduceError::InvalidRecord(
                            "queue cancellation has no matching entry".into(),
                        ));
                    };
                    lane.queued.remove(index);
                }
                Record::QueueConsumed {
                    run_id, entry_id, ..
                } => {
                    let Some(index) = lane.queued.iter().position(|queued| {
                        queued.target.id == *entry_id
                            && (queued.run_id.as_deref() == Some(run_id.as_str())
                                || (queued.run_id.is_none() && run_id.is_empty()))
                    }) else {
                        return Err(ReduceError::InvalidRecord(
                            "queue consumption has no matching entry".into(),
                        ));
                    };
                    lane.queued.remove(index);
                }
                Record::WriteDeferred { target, .. } => {
                    if target.id.trim().is_empty() {
                        return Err(ReduceError::InvalidRecord("empty deferred entry id".into()));
                    }
                    if let Some(parent_id) = target.parent_id.as_deref() {
                        if !entry_ids.contains(parent_id) {
                            return Err(ReduceError::MissingParent(parent_id.into()));
                        }
                    }
                    lane.deferred_writes.push(target.clone());
                }
                Record::WriteApplied { entry_id, .. } => {
                    let Some(index) = lane
                        .deferred_writes
                        .iter()
                        .position(|target| target.id == *entry_id)
                    else {
                        return Err(ReduceError::InvalidRecord(
                            "deferred write application has no pending target".into(),
                        ));
                    };
                    lane.deferred_writes.remove(index);
                }
                Record::FactSet { key, value, .. } => {
                    if key.trim().is_empty() {
                        return Err(ReduceError::InvalidRecord("empty fact key".into()));
                    }
                    lane.facts.insert(key.clone(), value.clone());
                }
                Record::HookResumeData { hook_id, data, .. } => {
                    if hook_id.trim().is_empty() {
                        return Err(ReduceError::InvalidRecord("empty hook id".into()));
                    }
                    lane.resume_data.insert(hook_id.clone(), data.clone());
                }
                Record::Usage {
                    usage,
                    cause: super::types::UsageCause::Provider,
                    run_id: Some(run_id),
                    attempt: Some(attempt),
                    ..
                } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    lane.usage.accumulate(usage);
                    lane.attempts = lane.attempts.max(*attempt);
                }
                Record::Usage { usage, run_id, .. } => {
                    if let Some(run_id) = run_id {
                        if lane.open_operation.as_deref() != Some(run_id) {
                            return Err(ReduceError::UnknownOperation(run_id.clone()));
                        }
                    }
                    lane.usage.accumulate(usage)
                }
                Record::ToolStarted {
                    id,
                    run_id,
                    assistant_entry_id,
                    tool_index,
                    tool_call_id,
                    tool_name,
                    result_entry_id,
                    replay,
                    ..
                } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    if tool_call_id.trim().is_empty()
                        || tool_name.trim().is_empty()
                        || result_entry_id.trim().is_empty()
                    {
                        return Err(ReduceError::InvalidRecord("empty tool result id".into()));
                    }
                    let Some(assistant) = store
                        .entries()
                        .iter()
                        .find(|entry| entry.id == *assistant_entry_id)
                    else {
                        return Err(ReduceError::InvalidRecord(
                            "tool intent references a missing assistant entry".into(),
                        ));
                    };
                    let declared = match &assistant.message {
                        crate::types::AgentMessage::Assistant {
                            tool_calls: Some(calls),
                            ..
                        } => calls.get(*tool_index).is_some_and(|call| {
                            call.id == *tool_call_id && call.function.name == *tool_name
                        }),
                        crate::types::AgentMessage::Assistant {
                            tool_calls: None, ..
                        } => true,
                        _ => false,
                    };
                    if !declared {
                        return Err(ReduceError::InvalidRecord(
                            "tool intent does not match assistant declaration".into(),
                        ));
                    }
                    let replay_claim = id.starts_with("replay-claim-")
                        && matches!(replay, super::types::ToolReplaySafety::Never);
                    if !replay_claim
                        && lane.tools.iter().any(|tool| {
                            tool.run_id == *run_id
                                && (tool.tool_call_id == *tool_call_id
                                    || (tool.assistant_entry_id == *assistant_entry_id
                                        && tool.tool_index == *tool_index))
                        })
                    {
                        return Err(ReduceError::InvalidRecord(
                            "tool intent duplicates call or ordinal".into(),
                        ));
                    }
                    if replay_claim {
                        lane.tools.retain(|tool| {
                            !(tool.run_id == *run_id
                                && tool.tool_call_id == *tool_call_id
                                && tool.assistant_entry_id == *assistant_entry_id
                                && tool.tool_index == *tool_index)
                        });
                    }
                    lane.tools.push(super::types::ToolState {
                        run_id: run_id.clone(),
                        assistant_entry_id: assistant_entry_id.clone(),
                        tool_index: *tool_index,
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        result_entry_id: result_entry_id.clone(),
                        replay: replay.clone(),
                        completed: false,
                        terminate: false,
                    });
                }
                Record::ToolFinished {
                    run_id,
                    tool_call_id,
                    result_entry_id,
                    terminate,
                    ..
                } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    let Some(tool_index) = lane.tools.iter().rposition(|tool| {
                        tool.run_id == *run_id
                            && tool.tool_call_id == *tool_call_id
                            && tool.result_entry_id == *result_entry_id
                    }) else {
                        return Err(ReduceError::InvalidRecord(
                            "tool completion has no matching intent".into(),
                        ));
                    };
                    let tool = &lane.tools[tool_index];
                    let Some(result_entry) = store
                        .entries()
                        .iter()
                        .find(|entry| entry.id == *result_entry_id)
                    else {
                        return Err(ReduceError::InvalidRecord(
                            "tool completion has no persisted result entry".into(),
                        ));
                    };
                    if !matches!(
                        &result_entry.message,
                        crate::types::AgentMessage::Tool {
                            tool_call_id: entry_call_id,
                            name,
                            ..
                        } if entry_call_id == tool_call_id && name == &tool.tool_name
                    ) || result_entry.parent_id.as_deref()
                        != Some(tool.assistant_entry_id.as_str())
                        || result_entry.terminate != *terminate
                    {
                        return Err(ReduceError::InvalidRecord(
                            "tool result entry does not match its intent".into(),
                        ));
                    }
                    if tool.completed {
                        return Err(ReduceError::InvalidRecord(
                            "tool completion is duplicated".into(),
                        ));
                    }
                    let tool = &mut lane.tools[tool_index];
                    tool.completed = true;
                    tool.terminate = *terminate;
                }
                Record::StepAttempt {
                    result_entry_id,
                    attempt,
                    run_id,
                    seq,
                    ..
                } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    let retry_is_current = store.records().iter().any(|record| {
                        matches!(
                            record,
                            Record::RetryConsumed {
                                run_id: consumed_run_id,
                                attempt: consumed_attempt,
                                seq: consumed_seq,
                                ..
                            } if consumed_run_id == run_id
                                && consumed_attempt == attempt
                                && consumed_seq < seq
                        )
                    }) && !store.records().iter().any(|record| {
                        matches!(
                            record,
                            Record::StepAttempt {
                                run_id: prior_run_id,
                                attempt: prior_attempt,
                                seq: prior_seq,
                                ..
                            } if prior_run_id == run_id
                                && prior_attempt == attempt
                                && prior_seq < seq
                        )
                    });
                    if *attempt != lane.attempts.saturating_add(1)
                        && !(retry_is_current && *attempt == lane.attempts)
                    {
                        return Err(ReduceError::InvalidRecord(
                            "step attempts must be consecutive".into(),
                        ));
                    }
                    lane.attempts = *attempt;
                    lane.retry = None;
                    if entry_ids.contains(result_entry_id.as_str()) {
                        lane.leaf_id = Some(result_entry_id.clone());
                    }
                }
                Record::RetryScheduled {
                    run_id,
                    timestamp,
                    attempt,
                    retry_at,
                    reason,
                    ..
                } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    if *attempt != lane.attempts.saturating_add(1)
                        || lane.retry.is_some()
                        || reason.trim().is_empty()
                    {
                        return Err(ReduceError::InvalidRecord(
                            "retry must be the next unscheduled attempt with a reason".into(),
                        ));
                    }
                    if *retry_at < *timestamp {
                        return Err(ReduceError::InvalidRecord(
                            "retry time cannot precede its durable record".into(),
                        ));
                    }
                    lane.retry = Some(super::types::RetryState {
                        attempt: *attempt,
                        retry_at: *retry_at,
                        reason: reason.clone(),
                    });
                }
                Record::RetryConsumed {
                    run_id, attempt, ..
                } => {
                    if lane.open_operation.as_deref() != Some(run_id) {
                        return Err(ReduceError::UnknownOperation(run_id.clone()));
                    }
                    let Some(retry) = lane.retry.take() else {
                        return Err(ReduceError::InvalidRecord(
                            "retry consumption has no scheduled retry".into(),
                        ));
                    };
                    if retry.attempt != *attempt {
                        return Err(ReduceError::InvalidRecord(
                            "retry consumption attempt does not match schedule".into(),
                        ));
                    }
                    lane.attempts = *attempt;
                }
            }
        }
        let mut lanes: Vec<_> = lanes.into_values().collect();
        for lane in &mut lanes {
            let Some(open_id) = lane.open_operation.as_deref() else {
                continue;
            };
            let start_seq = store.records().iter().find_map(|record| match record {
                Record::OperationStarted {
                    id,
                    lane: record_lane,
                    seq,
                    ..
                } if id == open_id && record_lane == &lane.name => Some(*seq),
                _ => None,
            });
            if start_seq.is_some_and(|start_seq| {
                let mut pending = false;
                for entry in store.entries().iter().filter(|entry| entry.seq > start_seq) {
                    if matches!(
                        &entry.message,
                        crate::types::AgentMessage::Assistant {
                            deferred_handle: Some(_),
                            ..
                        }
                    ) {
                        pending = true;
                    } else if pending
                        && matches!(
                            &entry.message,
                            crate::types::AgentMessage::Assistant {
                                deferred_handle: None,
                                ..
                            }
                        )
                    {
                        pending = false;
                    }
                }
                pending
            }) {
                lane.status = super::types::LaneStatus::SuspendedDeferred;
            }
        }
        lanes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ReducedState { lanes })
    }
}

impl Default for LaneState {
    fn default() -> Self {
        Self {
            name: String::new(),
            status: LaneStatus::Idle,
            leaf_id: None,
            open_operation: None,
            attempts: 0,
            retry: None,
            queued: Vec::new(),
            deferred_writes: Vec::new(),
            abort_requested: false,
            usage: Default::default(),
            tools: Vec::new(),
            facts: Default::default(),
            resume_data: Default::default(),
        }
    }
}
