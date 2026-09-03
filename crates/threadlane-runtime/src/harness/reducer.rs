use super::store::SessionStore;
use super::types::{
    Entry, LaneState, LaneStatus, OperationOutcome, QueuedEntry, Record, ReduceError, ReducedState,
    RetryState, ToolReplaySafety, ToolState,
};
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg(test)]
thread_local! {
    pub(super) static BUILD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub struct Reducer;

pub(crate) fn validate_candidate_entry<S: SessionStore>(
    store: &S,
    entry: &Entry,
) -> Result<(), ReduceError> {
    let context = ReductionContext::from_store(store)?;
    context.entry_guard(entry)
}

pub(crate) fn validate_candidate_record<S: SessionStore>(
    store: &S,
    record: &Record,
) -> Result<(), ReduceError> {
    let context = ReductionContext::from_store(store)?;
    context.record_guard(record)
}

/// Compact per-entry summary retained by [`ReductionContext`] so record
/// guards can validate against historical entries without borrowing into a
/// store's message payloads.
#[derive(Debug, Clone)]
struct EntryFacts {
    seq: u64,
    parent_id: Option<String>,
    /// `None` for non-assistant roles; `Some(None)` for an assistant without
    /// tool calls; `Some(Some((call_id, function_name)))` per declared call.
    assistant_calls: Option<Option<Vec<(String, String)>>>,
    /// `(tool_call_id, name)` when the message is a tool result role.
    tool_info: Option<(String, String)>,
    terminate: bool,
    deferred: DeferredKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredKind {
    AssistantDeferred,
    AssistantPlain,
    Other,
}

impl EntryFacts {
    fn capture(entry: &Entry) -> Self {
        Self {
            seq: entry.seq,
            parent_id: entry.parent_id.clone(),
            assistant_calls: match &entry.message {
                crate::types::AgentMessage::Assistant { tool_calls, .. } => {
                    Some(tool_calls.as_ref().map(|calls| {
                        calls
                            .iter()
                            .map(|call| (call.id.clone(), call.function.name.clone()))
                            .collect::<Vec<_>>()
                    }))
                }
                _ => None,
            },
            tool_info: match &entry.message {
                crate::types::AgentMessage::Tool {
                    tool_call_id, name, ..
                } => Some((tool_call_id.clone(), name.clone())),
                _ => None,
            },
            terminate: entry.terminate,
            deferred: deferred_kind_of(entry),
        }
    }
}

fn deferred_kind_of(entry: &Entry) -> DeferredKind {
    match &entry.message {
        crate::types::AgentMessage::Assistant {
            deferred_handle: Some(_),
            ..
        } => DeferredKind::AssistantDeferred,
        crate::types::AgentMessage::Assistant {
            deferred_handle: None,
            ..
        } => DeferredKind::AssistantPlain,
        _ => DeferredKind::Other,
    }
}

/// Auxiliary per-lane bookkeeping used only to answer incremental
/// check/commit queries in O(1); not part of the projected state.
#[derive(Debug, Clone, Default)]
struct LaneAux {
    incomplete_tools_by_run: HashMap<String, usize>,
    /// `(run_id, tool_call_id) -> index into lane.tools`.
    tool_by_call: HashMap<(String, String), usize>,
    /// `(run_id, assistant_entry_id, tool_index) -> index into lane.tools`.
    tool_by_ordinal: HashMap<(String, String, usize), usize>,
    queued_ids: HashSet<String>,
    pending_deferred: bool,
}

/// Streaming reducer core. Build once from full history (identical semantics
/// and error precedence to the historical single-pass reduce), then advance
/// one durable item at a time via guard/commit pairs so appends neither clone
/// history nor re-run a full reduction.
#[derive(Debug, Clone)]
pub(crate) struct ReductionContext {
    entry_ids: HashSet<String>,
    record_ids: HashSet<String>,
    seen_seqs: HashSet<u64>,
    entry_facts: HashMap<String, EntryFacts>,
    /// Per-lane `(seq, entry_id)` ascending by sequence.
    lane_entries: HashMap<String, Vec<(u64, String)>>,
    min_retry_consumed_seq: HashMap<(String, u32), u64>,
    min_step_attempt_seq: HashMap<(String, u32), u64>,
    has_v2_operation: bool,
    preferred_leaf_main: Option<String>,
    lanes: HashMap<String, LaneState>,
    aux: HashMap<String, LaneAux>,
}

impl ReductionContext {
    fn empty(fact_seed: BTreeMap<String, String>) -> Self {
        let mut lanes = HashMap::new();
        lanes.insert(
            "main".to_owned(),
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
                context_snapshots: Vec::new(),
                facts: fact_seed,
                resume_data: Default::default(),
            },
        );
        Self {
            entry_ids: HashSet::new(),
            record_ids: HashSet::new(),
            seen_seqs: HashSet::new(),
            entry_facts: HashMap::new(),
            lane_entries: HashMap::new(),
            min_retry_consumed_seq: HashMap::new(),
            min_step_attempt_seq: HashMap::new(),
            has_v2_operation: false,
            preferred_leaf_main: None,
            lanes,
            aux: HashMap::new(),
        }
    }

    /// Streams `entries` then `records`. Phase ordering mirrors the
    /// historical full reduce exactly: entry ids, record ids, sequence
    /// uniqueness, per-entry state application with parent/lane validation
    /// over the complete set, preferred-leaf override between streams, then
    /// per-record state transitions.
    pub(crate) fn build(
        entries: &[Entry],
        records: &[Record],
        fact_seed: BTreeMap<String, String>,
        preferred_leaf: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, ReduceError> {
        #[cfg(test)]
        BUILD_COUNT.with(|count| count.set(count.get() + 1));
        let mut ctx = Self::empty(fact_seed);
        for entry in entries {
            if entry.id.trim().is_empty() {
                return Err(ReduceError::InvalidRecord("empty entry id".into()));
            }
            if !ctx.entry_ids.insert(entry.id.clone()) {
                return Err(ReduceError::DuplicateId(entry.id.clone()));
            }
        }
        for record in records {
            if record.id().trim().is_empty() {
                return Err(ReduceError::InvalidRecord("empty record id".into()));
            }
            if ctx.entry_ids.contains(record.id()) || !ctx.record_ids.insert(record.id().to_owned())
            {
                return Err(ReduceError::DuplicateId(record.id().into()));
            }
        }
        ctx.seen_seqs.reserve(entries.len() + records.len());
        for seq in entries
            .iter()
            .map(|entry| entry.seq)
            .chain(records.iter().map(Record::seq))
        {
            if !ctx.seen_seqs.insert(seq) {
                return Err(ReduceError::NonMonotonicSequence {
                    previous: seq,
                    current: seq,
                });
            }
        }
        ctx.has_v2_operation = records
            .iter()
            .any(|record| matches!(record, Record::OperationStarted { .. }));

        // Entry streaming applies leaves and auxiliary flags. Parent and lane
        // validity are checked afterwards against the complete entry set so
        // forward references behave like the historical post-id-phase check.
        for entry in entries {
            ctx.commit_entry_inner(entry);
        }
        for entry in entries {
            if let Some(parent) = &entry.parent_id {
                if !ctx.entry_facts.contains_key(parent.as_str()) {
                    return Err(ReduceError::MissingParent(parent.clone()));
                }
            }
            if entry.lane.trim().is_empty() {
                return Err(ReduceError::InvalidLane(entry.lane.clone()));
            }
        }
        if !ctx.has_v2_operation {
            let names: Vec<String> = ctx.lanes.keys().cloned().collect();
            for name in names {
                if let Some(preferred) = preferred_leaf(&name) {
                    if !ctx.entry_ids.contains(&preferred) {
                        return Err(ReduceError::MissingParent(preferred));
                    }
                    ctx.lanes.get_mut(&name).unwrap().leaf_id = Some(preferred);
                }
            }
        }
        for record in records {
            ctx.record_guard(record)?;
            ctx.commit_record(record);
        }
        for ordered in ctx.lane_entries.values_mut() {
            ordered.sort_unstable_by_key(|(seq, _)| *seq);
        }
        Ok(ctx)
    }

    pub(crate) fn from_store<S: SessionStore>(store: &S) -> Result<Self, ReduceError> {
        Self::build(store.entries(), store.records(), store.facts(), &|lane| {
            store.preferred_leaf(lane)
        })
    }

    /// Projects the current reduced state. The suspended-deferred override
    /// for open lanes is applied here, mirroring the historical end-of-reduce
    /// scan.
    pub(crate) fn to_reduced_state(&self) -> ReducedState {
        let mut lanes: Vec<LaneState> = self.lanes.values().cloned().collect();
        for lane in &mut lanes {
            if lane.open_operation.is_some()
                && self
                    .aux
                    .get(&lane.name)
                    .is_some_and(|aux| aux.pending_deferred)
            {
                lane.status = LaneStatus::SuspendedDeferred;
            }
        }
        lanes.sort_by(|a, b| a.name.cmp(&b.name));
        ReducedState { lanes }
    }

    fn edit_lane<T>(&mut self, name: &str, f: impl FnOnce(&mut LaneState) -> T) -> T {
        let mut lane = self
            .lanes
            .entry(name.to_owned())
            .or_insert_with(|| LaneState {
                name: name.to_owned(),
                ..Default::default()
            });
        f(&mut lane)
    }

    fn edit_aux<T>(&mut self, name: &str, f: impl FnOnce(&mut LaneAux) -> T) -> T {
        let mut aux = self.aux.entry(name.to_owned()).or_default();
        f(&mut aux)
    }

    // ── Entries ───────────────────────────────────────────────────────

    /// Validates a candidate entry append without mutation. Error precedence
    /// mirrors the historical candidate reduce: id checks, sequence
    /// uniqueness, parent existence, lane validity.
    pub(crate) fn entry_guard(&self, entry: &Entry) -> Result<(), ReduceError> {
        self.candidate_entry_guard_inner(entry)?;
        if !self.has_v2_operation {
            if let Some(preferred) = &self.preferred_leaf_main {
                if !self.entry_ids.contains(preferred) {
                    return Err(ReduceError::MissingParent(preferred.clone()));
                }
            }
        }
        Ok(())
    }

    /// Guard checks for a candidate that is not yet registered in the id or
    /// sequence sets (unlike build-time streaming, where phases A–C already
    /// consumed the historical items).
    fn candidate_entry_guard_inner(&self, entry: &Entry) -> Result<(), ReduceError> {
        if entry.id.trim().is_empty() {
            return Err(ReduceError::InvalidRecord("empty entry id".into()));
        }
        if self.entry_ids.contains(&entry.id) || self.record_ids.contains(&entry.id) {
            return Err(ReduceError::DuplicateId(entry.id.clone()));
        }
        if self.seen_seqs.contains(&entry.seq) {
            return Err(ReduceError::NonMonotonicSequence {
                previous: entry.seq,
                current: entry.seq,
            });
        }
        if let Some(parent) = &entry.parent_id {
            if !self.entry_facts.contains_key(parent.as_str()) {
                return Err(ReduceError::MissingParent(parent.clone()));
            }
        }
        if entry.lane.trim().is_empty() {
            return Err(ReduceError::InvalidLane(entry.lane.clone()));
        }
        Ok(())
    }

    /// Applies a validated entry to the indexes and projected lane state.
    pub(crate) fn commit_entry(&mut self, entry: &Entry) {
        self.commit_entry_inner(entry);
    }

    fn commit_entry_inner(&mut self, entry: &Entry) {
        let deferred = deferred_kind_of(entry);
        let current_seq = self
            .lanes
            .get(&entry.lane)
            .and_then(|lane| lane.leaf_id.as_deref())
            .and_then(|leaf| self.entry_facts.get(leaf))
            .map(|facts| facts.seq);
        let apply_override = !self.has_v2_operation && entry.lane == "main";
        let preferred_leaf = self.preferred_leaf_main.clone();

        self.edit_lane(&entry.lane, |lane| {
            if current_seq.is_none_or(|current_seq| entry.seq >= current_seq) {
                lane.leaf_id = Some(entry.id.clone());
            }
            if apply_override {
                if let Some(preferred) = preferred_leaf {
                    lane.leaf_id = Some(preferred);
                }
            }
        });
        self.edit_aux(&entry.lane, |aux| match deferred {
            DeferredKind::AssistantDeferred => aux.pending_deferred = true,
            DeferredKind::AssistantPlain if aux.pending_deferred => {
                aux.pending_deferred = false;
            }
            _ => {}
        });
        self.entry_ids.insert(entry.id.clone());
        self.seen_seqs.insert(entry.seq);
        self.entry_facts
            .insert(entry.id.clone(), EntryFacts::capture(entry));
        let id = entry.id.clone();
        let seq = entry.seq;
        let lane_name = entry.lane.clone();
        self.lane_entries
            .entry(lane_name)
            .or_default()
            .push((seq, id));
    }

    /// Mirrors the store updating its preferred main leaf (e.g. after a
    /// main-lane entry append).
    pub(crate) fn set_preferred_leaf_main(&mut self, leaf: String) {
        self.preferred_leaf_main = Some(leaf);
    }

    // ── Records ───────────────────────────────────────────────────────

    /// Validates a candidate record append without mutation.
    pub(crate) fn record_guard(&self, record: &Record) -> Result<(), ReduceError> {
        let lane_name = record.lane().to_owned();
        if lane_name.trim().is_empty() {
            return Err(ReduceError::InvalidLane(lane_name));
        }
        let lane = self.lanes.get(&lane_name);
        let aux = self.aux.get(&lane_name);
        match record {
            Record::OperationStarted { source_leaf_id, .. } => {
                if let Some(leaf_id) = source_leaf_id {
                    if !self.entry_facts.contains_key(leaf_id.as_str()) {
                        return Err(ReduceError::MissingParent(leaf_id.clone()));
                    }
                }
                if lane.is_some_and(|lane| lane.open_operation.is_some()) {
                    return Err(ReduceError::MultipleOpenOperations(lane_name));
                }
            }
            Record::AbortRequested { run_id, .. } => {
                require_open(lane, run_id)?;
            }
            Record::OperationFinished { run_id, .. } => {
                require_open(lane, run_id)?;
                if aux
                    .and_then(|aux| aux.incomplete_tools_by_run.get(run_id))
                    .copied()
                    .unwrap_or(0)
                    > 0
                {
                    return Err(ReduceError::InvalidRecord(
                        "operation finished with an incomplete tool batch".into(),
                    ));
                }
                if lane.is_some_and(|lane| lane.retry.is_some()) {
                    return Err(ReduceError::InvalidRecord(
                        "operation finished while a retry is scheduled".into(),
                    ));
                }
            }
            Record::LaneMoved {
                run_id,
                target_leaf_id,
                ..
            } => {
                require_open(lane, run_id)?;
                if !self.entry_facts.contains_key(target_leaf_id.as_str()) {
                    return Err(ReduceError::MissingParent(target_leaf_id.clone()));
                }
            }
            Record::QueueEnqueued { target, .. } => {
                if target.id.trim().is_empty() {
                    return Err(ReduceError::InvalidRecord("empty queued entry id".into()));
                }
                if let Some(parent_id) = target.parent_id.as_deref() {
                    if !self.entry_facts.contains_key(parent_id) {
                        return Err(ReduceError::MissingParent(parent_id.into()));
                    }
                }
                if aux.is_some_and(|aux| aux.queued_ids.contains(&target.id)) {
                    return Err(ReduceError::InvalidRecord(
                        "queued entry is duplicated".into(),
                    ));
                }
            }
            Record::QueueCancelled {
                run_id, entry_id, ..
            } => {
                find_queued_index(lane, run_id, entry_id).ok_or_else(|| {
                    ReduceError::InvalidRecord("queue cancellation has no matching entry".into())
                })?;
            }
            Record::QueueConsumed {
                run_id, entry_id, ..
            } => {
                find_queued_index(lane, run_id, entry_id).ok_or_else(|| {
                    ReduceError::InvalidRecord("queue consumption has no matching entry".into())
                })?;
            }
            Record::WriteDeferred { target, .. } => {
                if target.id.trim().is_empty() {
                    return Err(ReduceError::InvalidRecord("empty deferred entry id".into()));
                }
                if let Some(parent_id) = target.parent_id.as_deref() {
                    if !self.entry_facts.contains_key(parent_id) {
                        return Err(ReduceError::MissingParent(parent_id.into()));
                    }
                }
            }
            Record::WriteApplied { entry_id, .. } => {
                let present = lane.is_some_and(|lane| {
                    lane.deferred_writes
                        .iter()
                        .any(|target| target.id == *entry_id)
                });
                if !present {
                    return Err(ReduceError::InvalidRecord(
                        "deferred write application has no pending target".into(),
                    ));
                }
            }
            Record::FactSet { key, .. } => {
                if key.trim().is_empty() {
                    return Err(ReduceError::InvalidRecord("empty fact key".into()));
                }
            }
            Record::HookResumeData { hook_id, .. } => {
                if hook_id.trim().is_empty() {
                    return Err(ReduceError::InvalidRecord("empty hook id".into()));
                }
            }
            Record::Usage {
                cause: super::types::UsageCause::Provider,
                run_id: Some(run_id),
                ..
            } => {
                require_open(lane, run_id)?;
            }
            Record::Usage { run_id, .. } => {
                if let Some(run_id) = run_id {
                    require_open(lane, run_id)?;
                }
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
                require_open(lane, run_id)?;
                if tool_call_id.trim().is_empty()
                    || tool_name.trim().is_empty()
                    || result_entry_id.trim().is_empty()
                {
                    return Err(ReduceError::InvalidRecord("empty tool result id".into()));
                }
                let assistant_facts = self.entry_facts.get(assistant_entry_id.as_str());
                let declared = match assistant_facts.map(|facts| &facts.assistant_calls) {
                    Some(Some(Some(calls))) => {
                        calls.get(*tool_index).is_some_and(|(call_id, name)| {
                            call_id == tool_call_id && name == tool_name
                        })
                    }
                    Some(Some(None)) => true,
                    _ => false,
                };
                if !declared {
                    return Err(ReduceError::InvalidRecord(
                        "tool intent does not match assistant declaration".into(),
                    ));
                }
                let replay_claim =
                    id.starts_with("replay-claim-") && matches!(replay, ToolReplaySafety::Never);
                if !replay_claim {
                    let duplicate_call = aux.is_some_and(|aux| {
                        aux.tool_by_call
                            .contains_key(&(run_id.to_owned(), tool_call_id.to_owned()))
                    });
                    let duplicate_ordinal = aux.is_some_and(|aux| {
                        aux.tool_by_ordinal.contains_key(&(
                            run_id.to_owned(),
                            assistant_entry_id.to_owned(),
                            *tool_index,
                        ))
                    });
                    if duplicate_call || duplicate_ordinal {
                        return Err(ReduceError::InvalidRecord(
                            "tool intent duplicates call or ordinal".into(),
                        ));
                    }
                }
            }
            Record::ToolFinished {
                run_id,
                tool_call_id,
                result_entry_id,
                terminate,
                ..
            } => {
                require_open(lane, run_id)?;
                let tools = lane.map(|lane| lane.tools.as_slice()).unwrap_or(&[]);
                let Some(tool_index) =
                    find_tool_index(tools, run_id, tool_call_id, result_entry_id)
                else {
                    return Err(ReduceError::InvalidRecord(
                        "tool completion has no matching intent".into(),
                    ));
                };
                let tool = &tools[tool_index];
                let Some(result_facts) = self.entry_facts.get(result_entry_id.as_str()) else {
                    return Err(ReduceError::InvalidRecord(
                        "tool completion has no persisted result entry".into(),
                    ));
                };
                let previous_result_parent = (tool.tool_index > 0)
                    .then(|| {
                        aux.and_then(|aux| {
                            aux.tool_by_ordinal
                                .get(&(
                                    run_id.to_owned(),
                                    tool.assistant_entry_id.clone(),
                                    tool.tool_index - 1,
                                ))
                                .and_then(|sibling| tools.get(*sibling))
                                .map(|sibling| sibling.result_entry_id.as_str())
                        })
                    })
                    .flatten();
                let parent_matches = result_facts.parent_id.as_deref()
                    == Some(tool.assistant_entry_id.as_str())
                    || result_facts.parent_id.as_deref() == previous_result_parent;
                let message_matches =
                    result_facts
                        .tool_info
                        .as_ref()
                        .is_some_and(|(call_id, name)| {
                            call_id == tool_call_id && name == &tool.tool_name
                        });
                if !message_matches || !parent_matches || result_facts.terminate != *terminate {
                    return Err(ReduceError::InvalidRecord(
                        "tool result entry does not match its intent".into(),
                    ));
                }
                if tool.completed {
                    return Err(ReduceError::InvalidRecord(
                        "tool completion is duplicated".into(),
                    ));
                }
                let _ = terminate;
            }
            Record::StepAttempt {
                attempt,
                run_id,
                seq,
                ..
            } => {
                require_open(lane, run_id)?;
                let key = (run_id.to_owned(), *attempt);
                let retry_is_current = self
                    .min_retry_consumed_seq
                    .get(&key)
                    .is_some_and(|consumed| *consumed < *seq)
                    && !self
                        .min_step_attempt_seq
                        .get(&key)
                        .is_some_and(|prior| *prior < *seq);
                let attempts = lane.map(|lane| lane.attempts).unwrap_or(0);
                if *attempt != attempts.saturating_add(1)
                    && !(retry_is_current && *attempt == attempts)
                {
                    return Err(ReduceError::InvalidRecord(
                        "step attempts must be consecutive".into(),
                    ));
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
                require_open(lane, run_id)?;
                let attempts = lane.map(|lane| lane.attempts).unwrap_or(0);
                let retry_scheduled = lane.is_some_and(|lane| lane.retry.is_some());
                if *attempt != attempts.saturating_add(1)
                    || retry_scheduled
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
            }
            Record::RetryConsumed {
                run_id, attempt, ..
            } => {
                require_open(lane, run_id)?;
                let retry_attempt = lane
                    .and_then(|lane| lane.retry.as_ref())
                    .map(|retry| retry.attempt);
                let Some(retry_attempt) = retry_attempt else {
                    return Err(ReduceError::InvalidRecord(
                        "retry consumption has no scheduled retry".into(),
                    ));
                };
                if retry_attempt != *attempt {
                    return Err(ReduceError::InvalidRecord(
                        "retry consumption attempt does not match schedule".into(),
                    ));
                }
            }
            Record::RunContextCaptured { run_id, .. }
            | Record::ContextManifestCaptured { run_id, .. }
            | Record::ContextCompacted { run_id, .. }
            | Record::ProviderRequestStarted { run_id, .. }
            | Record::ProviderRequestFinished { run_id, .. }
            | Record::ProviderResponseAttached { run_id, .. }
            | Record::ToolExecutionObserved { run_id, .. }
            | Record::AbortObserved { run_id, .. }
            | Record::StreamCheckpoint { run_id, .. } => {
                require_observation_open(lane, run_id)?;
            }
            Record::ContextSnapshotIndexed {
                run_id, snapshot, ..
            } => {
                if snapshot.context_id.trim().is_empty()
                    || snapshot.source_lane.trim().is_empty()
                    || snapshot.source_run_id.trim().is_empty()
                    || snapshot.source_tool_call_id.trim().is_empty()
                    || snapshot.source_entry_id.trim().is_empty()
                    || snapshot.path.trim().is_empty()
                    || snapshot.file_sha256.as_str().len() != 64
                    || !snapshot
                        .file_sha256
                        .as_str()
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(ReduceError::InvalidRecord(
                        "invalid context snapshot".into(),
                    ));
                }
                if snapshot.source_lane != lane_name || snapshot.source_run_id != *run_id {
                    return Err(ReduceError::InvalidRecord(
                        "context snapshot source does not match record".into(),
                    ));
                }
                let source_in_lane =
                    self.lane_entries
                        .get(&snapshot.source_lane)
                        .is_some_and(|entries| {
                            entries
                                .iter()
                                .any(|(_, id)| id == &snapshot.source_entry_id)
                        });
                let tool_matches = self
                    .entry_facts
                    .get(&snapshot.source_entry_id)
                    .and_then(|facts| facts.tool_info.as_ref())
                    .is_some_and(|(call_id, _)| call_id == &snapshot.source_tool_call_id);
                if !source_in_lane || !tool_matches {
                    return Err(ReduceError::InvalidRecord(
                        "context snapshot source entry does not match tool call".into(),
                    ));
                }
            }
            Record::ContextSnapshotLoaded {
                run_id,
                context_id,
                source_lane,
                current_digest,
                ..
            } => {
                if run_id.trim().is_empty()
                    || context_id.trim().is_empty()
                    || source_lane.trim().is_empty()
                    || current_digest.as_ref().is_some_and(|digest| {
                        digest.as_str().len() != 64
                            || !digest.as_str().bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                {
                    return Err(ReduceError::InvalidRecord(
                        "invalid context snapshot load".into(),
                    ));
                }
            }
            Record::PermissionRequested {
                run_id: Some(run_id),
                ..
            }
            | Record::PermissionResolved {
                run_id: Some(run_id),
                ..
            }
            | Record::SubagentLifecycle {
                run_id: Some(run_id),
                ..
            } => {
                require_observation_open(lane, run_id)?;
            }
            Record::PermissionRequested { run_id: None, .. }
            | Record::PermissionResolved { run_id: None, .. }
            | Record::SubagentLifecycle { run_id: None, .. } => {}
        }
        Ok(())
    }

    /// Applies a validated record to the projected lane state and indexes.
    pub(crate) fn commit_record(&mut self, record: &Record) {
        let lane_name = record.lane().to_owned();
        match record {
            Record::OperationStarted {
                id,
                seq,
                source_leaf_id,
                ..
            } => {
                let source_seq = source_leaf_id
                    .as_deref()
                    .and_then(|leaf| self.entry_facts.get(leaf))
                    .map(|facts| facts.seq);
                let source_leaf_id = source_leaf_id.clone();
                let current_seq = self
                    .lanes
                    .get(&lane_name)
                    .and_then(|lane| lane.leaf_id.as_deref())
                    .and_then(|leaf| self.entry_facts.get(leaf))
                    .map(|facts| facts.seq);
                let latest_beyond = self
                    .lane_entries
                    .get(&lane_name)
                    .and_then(|ordered| ordered.last())
                    .filter(|(latest_seq, _)| *latest_seq > *seq)
                    .map(|(_, latest_id)| latest_id.clone());
                let pending = self.scan_lane_pending(&lane_name, *seq);

                self.has_v2_operation = true;
                self.edit_lane(&lane_name, |lane| {
                    if let (Some(source_leaf_id), Some(source_seq)) =
                        (source_leaf_id.as_ref(), source_seq)
                    {
                        if current_seq.is_none_or(|current_seq| source_seq >= current_seq) {
                            lane.leaf_id = Some(source_leaf_id.clone());
                        }
                    }
                    if let Some(latest_beyond) = latest_beyond {
                        lane.leaf_id = Some(latest_beyond);
                    }
                    lane.open_operation = Some(id.clone());
                    lane.attempts = 0;
                    lane.retry = None;
                    lane.status = LaneStatus::SuspendedCrash;
                });
                self.edit_aux(&lane_name, |aux| aux.pending_deferred = pending);
            }
            Record::AbortRequested { .. } => {
                self.edit_lane(&lane_name, |lane| lane.abort_requested = true);
            }
            Record::OperationFinished { outcome, .. } => {
                self.edit_lane(&lane_name, |lane| {
                    lane.open_operation = None;
                    lane.abort_requested = false;
                    lane.retry = None;
                    lane.status = match outcome {
                        OperationOutcome::Completed => LaneStatus::Completed,
                        OperationOutcome::Failed => LaneStatus::Failed,
                        _ => LaneStatus::Idle,
                    };
                });
            }
            Record::LaneMoved { target_leaf_id, .. } => {
                let target_leaf_id = target_leaf_id.clone();
                self.edit_lane(&lane_name, |lane| lane.leaf_id = Some(target_leaf_id));
            }
            Record::QueueEnqueued {
                id,
                run_id,
                queue,
                priority,
                target,
                ..
            } => {
                let queued_entry = QueuedEntry {
                    id: id.clone(),
                    run_id: run_id.clone(),
                    queue: queue.clone(),
                    priority: *priority,
                    target: target.clone(),
                };
                let queued_target_id = target.id.clone();
                self.edit_lane(&lane_name, |lane| lane.queued.push(queued_entry));
                self.edit_aux(&lane_name, |aux| {
                    aux.queued_ids.insert(queued_target_id);
                });
            }
            Record::QueueCancelled {
                run_id, entry_id, ..
            } => {
                let found = find_queued_index(self.lanes.get(&lane_name), run_id, entry_id);
                if let Some(index) = found {
                    self.edit_lane(&lane_name, |lane| {
                        lane.queued.remove(index);
                    });
                }
                let entry_id = entry_id.clone();
                self.edit_aux(&lane_name, |aux| {
                    aux.queued_ids.remove(&entry_id);
                });
            }
            Record::QueueConsumed {
                run_id, entry_id, ..
            } => {
                let found = find_queued_index(self.lanes.get(&lane_name), run_id, entry_id);
                if let Some(index) = found {
                    self.edit_lane(&lane_name, |lane| {
                        lane.queued.remove(index);
                    });
                }
                let entry_id = entry_id.clone();
                self.edit_aux(&lane_name, |aux| {
                    aux.queued_ids.remove(&entry_id);
                });
            }
            Record::WriteDeferred { target, .. } => {
                let target = target.clone();
                self.edit_lane(&lane_name, |lane| lane.deferred_writes.push(target));
            }
            Record::WriteApplied { entry_id, .. } => {
                let found = self.lanes.get(&lane_name).and_then(|lane| {
                    lane.deferred_writes
                        .iter()
                        .position(|target| target.id == *entry_id)
                });
                if let Some(index) = found {
                    self.edit_lane(&lane_name, |lane| {
                        lane.deferred_writes.remove(index);
                    });
                }
            }
            Record::FactSet { key, value, .. } => {
                let key = key.clone();
                let value = value.clone();
                self.edit_lane(&lane_name, |lane| {
                    lane.facts.insert(key, value);
                });
            }
            Record::HookResumeData { hook_id, data, .. } => {
                let hook_id = hook_id.clone();
                let data = data.clone();
                self.edit_lane(&lane_name, |lane| {
                    lane.resume_data.insert(hook_id, data);
                });
            }
            Record::Usage {
                usage,
                cause: super::types::UsageCause::Provider,
                run_id: Some(_),
                attempt: Some(attempt),
                ..
            } => {
                let usage = usage.clone();
                let attempt = *attempt;
                self.edit_lane(&lane_name, |lane| {
                    lane.usage.accumulate(&usage);
                    lane.attempts = lane.attempts.max(attempt);
                });
            }
            Record::Usage { usage, .. } => {
                let usage = usage.clone();
                self.edit_lane(&lane_name, |lane| lane.usage.accumulate(&usage));
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
                let replay_claim =
                    id.starts_with("replay-claim-") && matches!(replay, ToolReplaySafety::Never);
                if replay_claim {
                    let quad_run = run_id.clone();
                    let quad_assistant = assistant_entry_id.clone();
                    let quad_index = *tool_index;
                    let quad_call = tool_call_id.clone();
                    let mut rebuilt = (HashMap::new(), HashMap::new());
                    let removed_incomplete = self.edit_lane(&lane_name, |lane| {
                        let removed_incomplete = lane
                            .tools
                            .iter()
                            .filter(|tool| {
                                same_tool_slot(
                                    tool,
                                    &quad_run,
                                    &quad_assistant,
                                    quad_index,
                                    &quad_call,
                                ) && !tool.completed
                            })
                            .count();
                        lane.tools.retain(|tool| {
                            !same_tool_slot(
                                tool,
                                &quad_run,
                                &quad_assistant,
                                quad_index,
                                &quad_call,
                            )
                        });
                        rebuilt = rebuild_tool_maps(&lane.tools);
                        removed_incomplete
                    });
                    self.edit_aux(&lane_name, |aux| {
                        *aux.incomplete_tools_by_run
                            .entry(run_id.to_owned())
                            .or_default() -= removed_incomplete;
                        aux.tool_by_call = rebuilt.0;
                        aux.tool_by_ordinal = rebuilt.1;
                    });
                }
                let new_index = self.edit_lane(&lane_name, |lane| {
                    lane.tools.push(ToolState {
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
                    lane.tools.len() - 1
                });
                self.edit_aux(&lane_name, |aux| {
                    *aux.incomplete_tools_by_run
                        .entry(run_id.to_owned())
                        .or_default() += 1;
                    aux.tool_by_call
                        .insert((run_id.to_owned(), tool_call_id.to_owned()), new_index);
                    aux.tool_by_ordinal.insert(
                        (
                            run_id.to_owned(),
                            assistant_entry_id.to_owned(),
                            *tool_index,
                        ),
                        new_index,
                    );
                });
            }
            Record::ToolFinished {
                run_id,
                tool_call_id,
                result_entry_id,
                terminate,
                ..
            } => {
                let updated = self.edit_lane(&lane_name, |lane| {
                    lane.tools
                        .iter()
                        .rposition(|tool| {
                            tool.run_id == *run_id
                                && tool.tool_call_id == *tool_call_id
                                && tool.result_entry_id == *result_entry_id
                        })
                        .map(|index| {
                            lane.tools[index].completed = true;
                            lane.tools[index].terminate = *terminate;
                        })
                });
                if updated.is_some() {
                    self.edit_aux(&lane_name, |aux| {
                        *aux.incomplete_tools_by_run
                            .entry(run_id.to_owned())
                            .or_default() -= 1;
                    });
                }
            }
            Record::StepAttempt {
                result_entry_id,
                attempt,
                run_id,
                seq,
                ..
            } => {
                self.min_step_attempt_seq_upsert((run_id.to_owned(), *attempt), *seq);
                let result_seq = self
                    .entry_facts
                    .get(result_entry_id.as_str())
                    .map(|facts| facts.seq)
                    .filter(|_| self.entry_facts.contains_key(result_entry_id.as_str()));
                let result_leaf = result_seq.map(|_| result_entry_id.clone());
                let current_seq = self
                    .lanes
                    .get(&lane_name)
                    .and_then(|lane| lane.leaf_id.as_deref())
                    .and_then(|leaf| self.entry_facts.get(leaf))
                    .map(|facts| facts.seq);
                self.edit_lane(&lane_name, |lane| {
                    lane.attempts = *attempt;
                    lane.retry = None;
                    if let (Some(result_leaf), Some(result_seq)) = (result_leaf, result_seq) {
                        if current_seq.is_none_or(|current_seq| result_seq >= current_seq) {
                            lane.leaf_id = Some(result_leaf);
                        }
                    }
                });
            }
            Record::RetryScheduled {
                attempt,
                retry_at,
                reason,
                ..
            } => {
                let retry_state = RetryState {
                    attempt: *attempt,
                    retry_at: *retry_at,
                    reason: reason.clone(),
                };
                self.edit_lane(&lane_name, |lane| lane.retry = Some(retry_state));
            }
            Record::RetryConsumed {
                run_id,
                attempt,
                seq,
                ..
            } => {
                self.min_retry_consumed_seq_upsert((run_id.to_owned(), *attempt), *seq);
                let attempt = *attempt;
                self.edit_lane(&lane_name, |lane| {
                    lane.retry = None;
                    lane.attempts = attempt;
                });
            }
            Record::RunContextCaptured { .. }
            | Record::ContextManifestCaptured { .. }
            | Record::ContextCompacted { .. }
            | Record::ProviderRequestStarted { .. }
            | Record::ProviderRequestFinished { .. }
            | Record::ProviderResponseAttached { .. }
            | Record::ToolExecutionObserved { .. }
            | Record::AbortObserved { .. }
            | Record::StreamCheckpoint { .. } => {}
            Record::ContextSnapshotIndexed { snapshot, .. } => {
                let snapshot = snapshot.clone();
                self.edit_lane(&lane_name, |lane| {
                    if let Some(index) = lane
                        .context_snapshots
                        .iter()
                        .position(|existing| existing.context_id == snapshot.context_id)
                    {
                        lane.context_snapshots[index] = snapshot;
                    } else {
                        lane.context_snapshots.push(snapshot);
                    }
                });
            }
            Record::ContextSnapshotLoaded { .. } => {}
            Record::PermissionRequested { .. }
            | Record::PermissionResolved { .. }
            | Record::SubagentLifecycle { .. } => {}
        }
    }

    fn min_retry_consumed_seq_upsert(&mut self, key: (String, u32), seq: u64) {
        self.min_retry_consumed_seq
            .entry(key)
            .and_modify(|current| *current = (*current).min(seq))
            .or_insert(seq);
    }

    fn min_step_attempt_seq_upsert(&mut self, key: (String, u32), seq: u64) {
        self.min_step_attempt_seq
            .entry(key)
            .and_modify(|current| *current = (*current).min(seq))
            .or_insert(seq);
    }

    /// Replays the historical deferred-pending scan for entries beyond
    /// `start_seq` on a lane: an assistant entry carrying a deferred handle
    /// arms the flag, a later plain assistant disarms it.
    fn scan_lane_pending(&self, lane_name: &str, start_seq: u64) -> bool {
        let Some(ordered) = self.lane_entries.get(lane_name) else {
            return false;
        };
        let start = ordered.partition_point(|(seq, _)| *seq <= start_seq);
        let mut pending = false;
        for (_, id) in &ordered[start..] {
            let Some(facts) = self.entry_facts.get(id) else {
                continue;
            };
            match facts.deferred {
                DeferredKind::AssistantDeferred => pending = true,
                DeferredKind::AssistantPlain if pending => pending = false,
                _ => {}
            }
        }
        pending
    }
}

fn same_tool_slot(
    tool: &ToolState,
    run_id: &str,
    assistant_entry_id: &str,
    tool_index: usize,
    tool_call_id: &str,
) -> bool {
    tool.run_id == run_id
        && tool.assistant_entry_id == assistant_entry_id
        && tool.tool_index == tool_index
        && tool.tool_call_id == tool_call_id
}

fn find_tool_index(
    tools: &[ToolState],
    run_id: &str,
    tool_call_id: &str,
    result_entry_id: &str,
) -> Option<usize> {
    tools.iter().rposition(|tool| {
        tool.run_id == run_id
            && tool.tool_call_id == tool_call_id
            && tool.result_entry_id == result_entry_id
    })
}

fn find_queued_index(lane: Option<&LaneState>, run_id: &str, entry_id: &str) -> Option<usize> {
    lane?.queued.iter().position(|queued| {
        queued.target.id == *entry_id
            && (queued.run_id.as_deref() == Some(run_id)
                || (queued.run_id.is_none() && run_id.is_empty()))
    })
}

fn rebuild_tool_maps(
    tools: &[ToolState],
) -> (
    HashMap<(String, String), usize>,
    HashMap<(String, String, usize), usize>,
) {
    let mut by_call = HashMap::new();
    let mut by_ordinal = HashMap::new();
    for (index, tool) in tools.iter().enumerate() {
        by_call.insert((tool.run_id.clone(), tool.tool_call_id.clone()), index);
        by_ordinal.insert(
            (
                tool.run_id.clone(),
                tool.assistant_entry_id.clone(),
                tool.tool_index,
            ),
            index,
        );
    }
    (by_call, by_ordinal)
}

fn require_open(lane: Option<&LaneState>, run_id: &str) -> Result<(), ReduceError> {
    if lane.is_some_and(|lane| lane.open_operation.as_deref() != Some(run_id)) {
        return Err(ReduceError::UnknownOperation(run_id.to_string()));
    }
    Ok(())
}

/// Observations before a retained start or after a retained finish remain
/// readable for compatibility; only accidental cross-run attribution fails.
fn require_observation_open(lane: Option<&LaneState>, run_id: &str) -> Result<(), ReduceError> {
    if let Some(open_run) = lane.and_then(|lane| lane.open_operation.as_deref()) {
        if open_run != run_id {
            return Err(ReduceError::UnknownOperation(run_id.to_string()));
        }
    }
    Ok(())
}

impl Reducer {
    pub fn reduce<S: SessionStore>(store: &S) -> Result<ReducedState, ReduceError> {
        if let Some(state) = store.reduced_state() {
            return Ok(state);
        }
        Ok(ReductionContext::from_store(store)?.to_reduced_state())
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
            context_snapshots: Vec::new(),
            facts: Default::default(),
            resume_data: Default::default(),
        }
    }
}
