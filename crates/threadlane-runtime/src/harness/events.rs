use super::store::SessionStore;
use super::types::{Entry, OperationIntent, OperationOutcome, Record, ReduceError, ReducedState};

use crate::types::TokenUsage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StreamingState {
    pub lane: String,
    pub run_id: Option<String>,
    pub assistant_text: String,
    pub reasoning: String,
    pub tool_call_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub session_id: String,
    pub state: ReducedState,
    pub entries: Vec<Entry>,
    pub records: Vec<Record>,
    #[serde(default)]
    pub streaming: Option<StreamingState>,
}

impl Snapshot {
    pub fn from_store<S: SessionStore>(store: &S) -> Result<Self, ReduceError> {
        Ok(Self {
            session_id: store.session_id().into(),
            state: super::Reducer::reduce(store)?,
            entries: store.entries().to_vec(),
            records: store.records().to_vec(),
            streaming: None,
        })
    }

    /// Returns `true` when non-main lanes contain at least one operation
    /// that has started but not yet finished.
    pub fn has_open_subagent_lanes(&self) -> bool {
        self.state
            .lanes
            .iter()
            .any(|lane| lane.name != "main" && lane.open_operation.is_some())
    }

    /// Reconstructs open/interrupted subagent lanes from raw records.
    pub fn interrupted_subagent_lanes(&self) -> Vec<super::types::InterruptedSubagentLane> {
        interrupted_subagent_lanes(&self.records)
    }
}

/// Returns `true` when non-main-lane records contain at least one
/// operation that has started but not yet finished.
pub fn has_open_subagent_lanes(records: &[Record]) -> bool {
    let finished: std::collections::HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Record::OperationFinished { run_id, .. } => Some(run_id.as_str()),
            _ => None,
        })
        .collect();
    records.iter().any(|r| match r {
        Record::OperationStarted { id, lane, .. } => {
            lane != "main" && !finished.contains(id.as_str())
        }
        _ => false,
    })
}

/// Reconstructs open/interrupted subagent lanes from raw records.
pub fn interrupted_subagent_lanes(
    records: &[Record],
) -> Vec<super::types::InterruptedSubagentLane> {
    use crate::harness::ToolReplaySafety;
    use crate::types::AgentMessage;

    struct Occurrence {
        lane: String,
        run_id: String,
        started_seq: u64,
        source_leaf_id: Option<String>,
        task: String,
        task_attempted: bool,
        messages: Vec<(u64, AgentMessage)>,
        tools: Vec<Record>,
        completed_tools: std::collections::HashSet<String>,
        active: bool,
    }

    let mut ordered: Vec<_> = records.iter().enumerate().collect();
    ordered.sort_by_key(|(index, record)| (record.seq(), *index));
    let mut occurrences = Vec::new();
    let mut active: HashMap<(String, String), Vec<usize>> = HashMap::new();

    for (_, record) in ordered {
        match record {
            Record::OperationStarted {
                id,
                lane,
                seq,
                source_leaf_id,
                intent: OperationIntent::Run,
                ..
            } => {
                let index = occurrences.len();
                occurrences.push(Occurrence {
                    lane: lane.clone(),
                    run_id: id.clone(),
                    started_seq: *seq,
                    source_leaf_id: source_leaf_id.clone(),
                    task: String::new(),
                    task_attempted: false,
                    messages: Vec::new(),
                    tools: Vec::new(),
                    completed_tools: std::collections::HashSet::new(),
                    active: true,
                });
                active
                    .entry((lane.clone(), id.clone()))
                    .or_default()
                    .push(index);
            }
            Record::StepAttempt { lane, run_id, .. } => {
                if let Some(index) = active
                    .get(&(lane.clone(), run_id.clone()))
                    .and_then(|occurrences| occurrences.last())
                {
                    occurrences[*index].task_attempted = true;
                }
            }
            Record::WriteDeferred {
                lane,
                run_id,
                seq,
                target,
                ..
            } => {
                if let Some(index) = active
                    .get(&(lane.clone(), run_id.clone()))
                    .and_then(|occurrences| occurrences.last())
                {
                    occurrences[*index]
                        .messages
                        .push((*seq, target.message.clone()));
                }
            }
            Record::ToolStarted { lane, run_id, .. } => {
                if let Some(index) = active
                    .get(&(lane.clone(), run_id.clone()))
                    .and_then(|occurrences| occurrences.last())
                {
                    occurrences[*index].tools.push(record.clone());
                }
            }
            Record::ToolFinished {
                lane,
                run_id,
                tool_call_id,
                ..
            } => {
                if let Some(index) = active
                    .get(&(lane.clone(), run_id.clone()))
                    .and_then(|occurrences| occurrences.last())
                {
                    occurrences[*index]
                        .completed_tools
                        .insert(tool_call_id.clone());
                }
            }
            Record::OperationFinished { lane, run_id, .. } => {
                let key = (lane.clone(), run_id.clone());
                let remove_key = if let Some(occurrences_for_run) = active.get_mut(&key) {
                    if let Some(index) = occurrences_for_run.pop() {
                        occurrences[index].active = false;
                    }
                    occurrences_for_run.is_empty()
                } else {
                    false
                };
                if remove_key {
                    active.remove(&key);
                }
            }
            _ => {}
        }
    }

    let mut lanes = Vec::new();
    for occurrence in occurrences
        .into_iter()
        .filter(|occurrence| occurrence.active)
    {
        let mut messages = occurrence.messages;
        let mut completed_tool_calls: std::collections::HashSet<String> = messages
            .iter()
            .filter_map(|(_, message)| match message {
                AgentMessage::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect();
        completed_tool_calls.extend(occurrence.completed_tools);
        let mut tools: HashMap<String, (Option<Record>, Option<Record>)> = HashMap::new();

        for record in occurrence.tools {
            let Record::ToolStarted {
                tool_call_id,
                replay,
                ..
            } = &record
            else {
                continue;
            };
            if completed_tool_calls.contains(tool_call_id) {
                continue;
            }
            let entry = tools.entry(tool_call_id.clone()).or_default();
            match replay {
                ToolReplaySafety::Safe if entry.0.is_none() => entry.0 = Some(record.clone()),
                ToolReplaySafety::Never if entry.1.is_none() => entry.1 = Some(record.clone()),
                _ => {}
            }
        }

        let mut safe_tools = Vec::new();
        let mut unsafe_tools = Vec::new();
        for (_, (safe, never)) in tools {
            if let Some(record) = never {
                if let Record::ToolStarted {
                    seq,
                    tool_call_id,
                    tool_name,
                    ..
                } = &record
                {
                    messages.push((
                        *seq,
                        AgentMessage::Tool {
                            tool_call_id: tool_call_id.clone(),
                            name: tool_name.clone(),
                            content: format!(
                                "[Interrupted tool execution for '{tool_name}' automatically recovered]"
                            ),
                            is_error: true,
                            terminate: false,
                        },
                    ));
                }
                unsafe_tools.push(record);
            } else if let Some(record) = safe {
                safe_tools.push(record);
            }
        }
        messages.sort_by_key(|(seq, _)| *seq);
        safe_tools.sort_by_key(Record::seq);
        unsafe_tools.sort_by_key(Record::seq);

        let task = if occurrence.task.is_empty() {
            messages
                .iter()
                .find_map(|(_, msg)| match msg {
                    AgentMessage::User { content } => Some(content.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        } else {
            occurrence.task
        };

        lanes.push((
            occurrence.started_seq,
            super::types::InterruptedSubagentLane {
                lane: occurrence.lane,
                run_id: occurrence.run_id,
                source_leaf_id: occurrence.source_leaf_id,
                started_seq: occurrence.started_seq,
                task,
                task_attempted: occurrence.task_attempted,
                messages: messages.into_iter().map(|(_, message)| message).collect(),
                safe_tools,
                unsafe_tools,
            },
        ));
    }

    lanes.sort_by_key(|(started_seq, _)| *started_seq);
    lanes.into_iter().map(|(_, lane)| lane).collect()
}

/// The typed payload of a durable event in the append-only journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DurablePayload {
    Entry(Entry),
    Record(Record),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventPayload {
    EntryCommitted(Entry),
    RecordCommitted(Record),
    Fault(String),
    Streaming(Option<StreamingState>),
    Agent(crate::events::AgentEvent),
}

impl EventPayload {
    /// Returns `true` if this payload represents a committed journal fact.
    pub fn is_durable(&self) -> bool {
        matches!(
            self,
            EventPayload::EntryCommitted(_) | EventPayload::RecordCommitted(_)
        )
    }

    /// Returns `true` if this payload represents a live/transient event.
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            EventPayload::Streaming(_) | EventPayload::Agent(_) | EventPayload::Fault(_)
        )
    }

    /// Converts this payload into a [`DurablePayload`] if it is durable.
    pub fn as_durable(&self) -> Option<DurablePayload> {
        match self {
            EventPayload::EntryCommitted(entry) => Some(DurablePayload::Entry(entry.clone())),
            EventPayload::RecordCommitted(record) => Some(DurablePayload::Record(record.clone())),
            _ => None,
        }
    }
}

/// An explicit representation of a durable event in the append-only journal.
///
/// Durable events represent committed facts on disk (entries and records) and can
/// be deterministically projected into model context, UI transcript, or compatibility
/// lifecycle `AgentEvent`s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurableEvent {
    /// The durable commit cursor (`HarnessEvent::id`).
    pub cursor: u64,
    pub payload: DurablePayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_intent: Option<OperationIntent>,
}

impl DurableEvent {
    /// Returns the sequence number of the underlying entry or record.
    pub fn seq(&self) -> u64 {
        match &self.payload {
            DurablePayload::Entry(e) => e.seq,
            DurablePayload::Record(r) => r.seq(),
        }
    }

    /// Returns `true` if this durable event wraps an [`Entry`].
    pub fn is_entry(&self) -> bool {
        matches!(self.payload, DurablePayload::Entry(_))
    }

    /// Returns `true` if this durable event wraps a [`Record`].
    pub fn is_record(&self) -> bool {
        matches!(self.payload, DurablePayload::Record(_))
    }

    /// Returns a reference to the inner [`Entry`], if this is an entry event.
    pub fn entry(&self) -> Option<&Entry> {
        match &self.payload {
            DurablePayload::Entry(e) => Some(e),
            _ => None,
        }
    }

    /// Returns a reference to the inner [`Record`], if this is a record event.
    pub fn record(&self) -> Option<&Record> {
        match &self.payload {
            DurablePayload::Record(r) => Some(r),
            _ => None,
        }
    }

    /// Project this durable event into an [`AgentEvent`] compatibility lifecycle event.
    pub fn project_agent_event(&self) -> Option<crate::events::AgentEvent> {
        match &self.payload {
            DurablePayload::Entry(entry) => Some(crate::events::AgentEvent::MessageEnd {
                message: entry.message.clone(),
            }),
            DurablePayload::Record(record) => match record {
                Record::OperationStarted { intent, .. } if *intent == OperationIntent::Run => {
                    Some(crate::events::AgentEvent::AgentStart)
                }
                Record::OperationFinished { outcome, error, .. }
                    if self.operation_intent == Some(OperationIntent::Run) =>
                {
                    match outcome {
                        OperationOutcome::Completed => Some(crate::events::AgentEvent::AgentEnd {
                            usage: TokenUsage::default(),
                        }),
                        _ => Some(crate::events::AgentEvent::AgentError {
                            error: error.clone().unwrap_or_else(|| match outcome {
                                OperationOutcome::Failed => "operation failed".to_string(),
                                OperationOutcome::Aborted => "operation aborted".to_string(),
                                OperationOutcome::Declined => "operation declined".to_string(),
                                _ => unreachable!(),
                            }),
                        }),
                    }
                }
                Record::StepAttempt { attempt, .. } => Some(crate::events::AgentEvent::TurnStart {
                    turn_number: *attempt as usize,
                }),
                Record::ToolStarted {
                    tool_call_id,
                    tool_name,
                    effective_args,
                    ..
                } => Some(crate::events::AgentEvent::ToolExecutionStart {
                    tool_call_id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    arguments: serde_json::to_string(effective_args).unwrap_or_default(),
                }),
                _ => None,
            },
        }
    }

    /// Project into a [`ProjectedAgentEvent`] carrying the commit cursor and
    /// identity fields alongside the inner agent event.
    pub fn project(&self) -> Option<ProjectedAgentEvent> {
        self.project_agent_event().map(|event| ProjectedAgentEvent {
            cursor: self.cursor,
            lane: self.lane.clone(),
            run_id: self.run_id.clone(),
            turn: self.turn,
            recovery_id: self.recovery_id.clone(),
            event,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessEvent {
    pub id: u64,
    pub payload: EventPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_id: Option<String>,
    /// The correlated [`OperationIntent`] for an [`EventPayload::RecordCommitted`]
    /// that wraps a [`Record::OperationFinished`]; resolved by the event hub from
    /// the matching [`Record::OperationStarted`].  `None` for other payloads and
    /// for finished operations whose start was not observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation_intent: Option<OperationIntent>,
}

/// A compatibility projection of a [`HarnessEvent`] into an [`AgentEvent`],
/// carrying the commit cursor and identity fields for downstream consumers
/// that need lane/run/turn context without depending on the full harness event.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedAgentEvent {
    /// The durable commit cursor (`HarnessEvent::id`).
    pub cursor: u64,
    pub lane: Option<String>,
    pub run_id: Option<String>,
    pub turn: Option<u32>,
    /// The recovery identifier from the originating [`HarnessEvent`].
    pub recovery_id: Option<String>,
    pub event: crate::events::AgentEvent,
}

impl HarnessEvent {
    /// Returns `true` if this event represents a committed journal fact.
    pub fn is_durable(&self) -> bool {
        self.payload.is_durable()
    }

    /// Returns `true` if this event represents a live/transient event.
    pub fn is_live(&self) -> bool {
        self.payload.is_live()
    }

    /// Converts this event into a [`DurableEvent`] if its payload is durable.
    pub fn as_durable(&self) -> Option<DurableEvent> {
        let payload = self.payload.as_durable()?;
        Some(DurableEvent {
            cursor: self.id,
            payload,
            lane: self.lane.clone(),
            run_id: self.run_id.clone(),
            turn: self.turn,
            recovery_id: self.recovery_id.clone(),
            operation_intent: self.operation_intent.clone(),
        })
    }

    /// Project this harness event into an [`AgentEvent`] from committed
    /// [`EventPayload::EntryCommitted`] and [`EventPayload::RecordCommitted`]
    /// payloads.  Ephemeral [`EventPayload::Agent`] payloads (raw TurnDriver
    /// streaming events) are **never** projected — only durable commit records
    /// yield lifecycle-compatible events.
    ///
    /// | Committed payload | AgentEvent |
    /// |---|---|
    /// | `EntryCommitted` | `MessageEnd` with the entry's `message` |
    /// | `RecordCommitted::OperationStarted` (`Run`) | `AgentStart` |
    /// | `RecordCommitted::OperationFinished` (when `operation_intent` is `Run`) | `AgentEnd` / `AgentError` |
    /// | `RecordCommitted::StepAttempt` | `TurnStart` with `attempt` |
    /// | `RecordCommitted::ToolStarted` | `ToolExecutionStart` with JSON-serialized `effective_args` |
    /// | Everything else | `None` |
    pub fn project_agent_event(&self) -> Option<crate::events::AgentEvent> {
        self.as_durable().and_then(|d| d.project_agent_event())
    }

    /// Project into a [`ProjectedAgentEvent`] carrying the commit cursor and
    /// identity fields alongside the inner agent event.
    pub fn project(&self) -> Option<ProjectedAgentEvent> {
        self.as_durable().and_then(|d| d.project())
    }

    /// Short label for logging the payload variant.
    pub fn payload_variant(&self) -> &'static str {
        match &self.payload {
            EventPayload::EntryCommitted(_) => "EntryCommitted",
            EventPayload::RecordCommitted(r) => match r {
                Record::OperationStarted { .. } => "OperationStarted",
                Record::OperationFinished { .. } => "OperationFinished",
                Record::StepAttempt { .. } => "StepAttempt",
                Record::ToolStarted { .. } => "ToolStarted",
                Record::AbortRequested { .. } => "AbortRequested",
                Record::LaneMoved { .. } => "LaneMoved",
                Record::RunContextCaptured { .. } => "RunContextCaptured",
                Record::ContextManifestCaptured { .. } => "ContextManifestCaptured",
                Record::ContextSnapshotIndexed { .. } => "ContextSnapshotIndexed",
                Record::ContextSnapshotLoaded { .. } => "ContextSnapshotLoaded",
                Record::ProviderRequestStarted { .. } => "ProviderRequestStarted",
                Record::ProviderRequestFinished { .. } => "ProviderRequestFinished",
                Record::ProviderResponseAttached { .. } => "ProviderResponseAttached",
                Record::PermissionRequested { .. } => "PermissionRequested",
                Record::PermissionResolved { .. } => "PermissionResolved",
                Record::ToolExecutionObserved { .. } => "ToolExecutionObserved",
                Record::AbortObserved { .. } => "AbortObserved",
                Record::SubagentLifecycle { .. } => "SubagentLifecycle",
                Record::StreamCheckpoint { .. } => "StreamCheckpoint",
                _ => "Record(…)",
            },
            EventPayload::Fault(_) => "Fault",
            EventPayload::Streaming(_) => "Streaming",
            EventPayload::Agent(_) => "Agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventError {
    Gap { requested: u64, oldest: u64 },
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub snapshot: Snapshot,
    next_id: u64,
    lane: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HarnessEventHub {
    inner: Arc<Mutex<HarnessEventHubState>>,
    notify: Arc<Notify>,
}

#[derive(Debug)]
struct HarnessEventHubState {
    capacity: usize,
    next_id: u64,
    events: VecDeque<HarnessEvent>,
    streaming: Option<StreamingState>,
    /// Tracks the intent of every observed `OperationStarted` record keyed by
    /// `(lane, run_id)` so that the matching `OperationFinished` can carry it.
    operation_intents: HashMap<(String, String), OperationIntent>,
}

/// Hydrate `operation_intents` from store records so a fresh hub after restart
/// can still correlate a later `OperationFinished` with its original intent.
///
/// Only *currently open* operations are hydrated — when the store contains
/// both an `OperationStarted` and a corresponding `OperationFinished` the
/// intent is cleared and a duplicate finish will not project.
fn hydrate_intents_from_store<S: SessionStore>(
    intents: &mut HashMap<(String, String), OperationIntent>,
    store: &S,
) {
    // First pass: collect every OperationStarted intent.
    for record in store.records() {
        if let Record::OperationStarted {
            intent, lane, id, ..
        } = record
        {
            intents.insert((lane.clone(), id.clone()), intent.clone());
        }
    }
    // Second pass: remove closed operations — those with a persisted
    // OperationFinished.
    for record in store.records() {
        if let Record::OperationFinished { lane, run_id, .. } = record {
            intents.remove(&(lane.clone(), run_id.clone()));
        }
    }
}

impl HarnessEventHub {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HarnessEventHubState {
                capacity: capacity.max(1),
                next_id: 1,
                events: VecDeque::new(),
                streaming: None,
                operation_intents: HashMap::new(),
            })),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn publish(&self, payload: EventPayload) -> HarnessEvent {
        self.publish_identified(payload, None, None, None)
    }

    pub fn publish_agent_event(&self, event: crate::events::AgentEvent) -> HarnessEvent {
        self.publish(EventPayload::Agent(event))
    }

    pub fn publish_identified(
        &self,
        payload: EventPayload,
        lane: Option<String>,
        run_id: Option<String>,
        recovery_id: Option<String>,
    ) -> HarnessEvent {
        self.publish_identified_with_turn(payload, lane, run_id, None, recovery_id)
    }

    pub fn publish_identified_with_turn(
        &self,
        payload: EventPayload,
        lane: Option<String>,
        run_id: Option<String>,
        turn: Option<u32>,
        recovery_id: Option<String>,
    ) -> HarnessEvent {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());

        // Resolve the operation intent: store for OperationStarted, look up for
        // OperationFinished.  Use the provided identity fields when they are set,
        // falling back to the record's own lane / run_id so that both the
        // harness-integrated path (publish_identified) and direct test publication
        // (publish) produce correct correlation.
        let mut operation_intent = None;
        if let EventPayload::RecordCommitted(record) = &payload {
            let effective_lane = lane.clone().unwrap_or_else(|| record.lane().to_owned());
            let effective_run: Option<String> = run_id
                .clone()
                .or_else(|| record.run_id().map(str::to_owned));
            match record {
                Record::OperationStarted { intent, .. } => {
                    if let Some(r) = &effective_run {
                        state
                            .operation_intents
                            .insert((effective_lane, r.clone()), intent.clone());
                    }
                }
                Record::OperationFinished { .. } => {
                    if let Some(r) = &effective_run {
                        operation_intent = state
                            .operation_intents
                            .get(&(effective_lane.clone(), r.clone()))
                            .cloned();
                        // Remove the spent intent so per-run memory stays bounded.
                        state.operation_intents.remove(&(effective_lane, r.clone()));
                    }
                }
                _ => {}
            }
        }

        let event = HarnessEvent {
            id: state.next_id,
            payload,
            lane,
            run_id,
            turn,
            recovery_id,
            operation_intent,
        };
        state.next_id += 1;
        if state.events.len() == state.capacity {
            state.events.pop_front();
        }
        state.events.push_back(event.clone());
        drop(state);
        self.notify.notify_waiters();
        event
    }

    pub fn publish_streaming(&self, state: Option<StreamingState>) -> HarnessEvent {
        let (lane, run_id) = state
            .as_ref()
            .map(|state| (Some(state.lane.clone()), state.run_id.clone()))
            .unwrap_or((None, None));
        let mut hub = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        hub.streaming = state.clone();
        let event = HarnessEvent {
            id: hub.next_id,
            payload: EventPayload::Streaming(state),
            lane,
            run_id,
            turn: None,
            recovery_id: None,
            operation_intent: None,
        };
        hub.next_id += 1;
        if hub.events.len() == hub.capacity {
            hub.events.pop_front();
        }
        hub.events.push_back(event.clone());
        drop(hub);
        self.notify.notify_waiters();
        event
    }

    pub(crate) fn streaming_state(&self) -> Option<StreamingState> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .streaming
            .clone()
    }

    pub fn subscribe<S: SessionStore>(&self, store: &S) -> Result<Subscription, ReduceError> {
        self.subscribe_for_lane(store, None)
    }

    pub fn subscribe_for_lane<S: SessionStore>(
        &self,
        store: &S,
        lane: Option<&str>,
    ) -> Result<Subscription, ReduceError> {
        // Keep the cursor paired with the snapshot. Commits cannot publish
        // between these two observations, so polling starts without a gap.
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let mut snapshot = Snapshot::from_store(store)?;
        snapshot.streaming = state
            .streaming
            .clone()
            .filter(|stream| lane.is_none_or(|lane| stream.lane == lane));

        // Hydrate operation_intents from existing store records so a fresh
        // hub after restart can correlate a later OperationFinished with its
        // original OperationStarted intent.
        hydrate_intents_from_store(&mut state.operation_intents, store);

        Ok(Subscription {
            snapshot,
            next_id: state.next_id,
            lane: lane.map(str::to_owned),
        })
    }

    pub fn publish_durable(
        &self,
        payload: DurablePayload,
        lane: Option<String>,
        run_id: Option<String>,
        recovery_id: Option<String>,
    ) -> HarnessEvent {
        let event_payload = match payload {
            DurablePayload::Entry(entry) => EventPayload::EntryCommitted(entry),
            DurablePayload::Record(record) => EventPayload::RecordCommitted(record),
        };
        self.publish_identified(event_payload, lane, run_id, recovery_id)
    }

    pub fn poll(&self, subscription: &mut Subscription) -> Result<Vec<HarnessEvent>, EventError> {
        let state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let Some(oldest) = state.events.front().map(|event| event.id) else {
            return Ok(Vec::new());
        };
        if subscription.next_id < oldest {
            return Err(EventError::Gap {
                requested: subscription.next_id,
                oldest,
            });
        }
        let events: Vec<_> = state
            .events
            .iter()
            .filter(|event| {
                event.id >= subscription.next_id
                    && subscription
                        .lane
                        .as_deref()
                        .is_none_or(|lane| event.lane.as_deref() == Some(lane))
            })
            .cloned()
            .collect();
        if let Some(last_seen) = state
            .events
            .iter()
            .filter(|event| event.id >= subscription.next_id)
            .map(|event| event.id)
            .next_back()
        {
            subscription.next_id = last_seen + 1;
        }
        Ok(events)
    }

    pub async fn wait(
        &self,
        subscription: &mut Subscription,
    ) -> Result<Vec<HarnessEvent>, EventError> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let events = self.poll(subscription)?;
            if !events.is_empty() {
                return Ok(events);
            }
            notified.await;
        }
    }

    /// Polls only durable events (entry/record commits) from the subscription.
    pub fn poll_durable(
        &self,
        subscription: &mut Subscription,
    ) -> Result<Vec<DurableEvent>, EventError> {
        let events = self.poll(subscription)?;
        Ok(events.into_iter().filter_map(|e| e.as_durable()).collect())
    }

    pub fn unsubscribe(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{MemoryStore, SurfaceOperation};
    use crate::types::AgentMessage;

    #[tokio::test]
    async fn subscription_waits_for_publication_without_polling() {
        let hub = HarnessEventHub::new(8);
        let store = MemoryStore::new("session");
        let mut subscription = hub.subscribe(&store).unwrap();
        let publisher = hub.clone();

        tokio::spawn(async move {
            tokio::task::yield_now().await;
            publisher.publish_agent_event(crate::events::AgentEvent::AgentStart);
        });

        let events = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            hub.wait(&mut subscription),
        )
        .await
        .expect("publication should wake the subscription")
        .unwrap();

        assert_eq!(events.len(), 1);
    }

    #[test]
    fn durable_event_identification_and_projection() {
        let entry = Entry {
            id: "entry-1".into(),
            parent_id: None,
            seq: 1,
            lane: "main".into(),
            timestamp: 1000,
            message: AgentMessage::user("hello world", Vec::new()),
            surface_op: SurfaceOperation::Append,
            terminate: false,
        };

        let durable_entry = DurableEvent {
            cursor: 1,
            payload: DurablePayload::Entry(entry),
            lane: Some("main".into()),
            run_id: Some("run-1".into()),
            turn: Some(1),
            recovery_id: None,
            operation_intent: Some(OperationIntent::Run),
        };

        assert!(durable_entry.is_entry());
        assert!(!durable_entry.is_record());
        assert_eq!(durable_entry.seq(), 1);
        assert!(durable_entry.entry().is_some());
        assert!(durable_entry.record().is_none());

        let projected = durable_entry.project();
        assert!(projected.is_some());
        let projected = projected.unwrap();
        assert_eq!(projected.cursor, 1);
        assert_eq!(projected.lane.as_deref(), Some("main"));
        assert_eq!(projected.run_id.as_deref(), Some("run-1"));
        assert!(matches!(
            projected.event,
            crate::events::AgentEvent::MessageEnd { .. }
        ));

        let step_record = Record::StepAttempt {
            id: "step-1".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 1001,
            run_id: "run-1".into(),
            attempt: 2,
            result_entry_id: "entry-2".into(),
            compaction_reason: None,
        };

        let durable_step = DurableEvent {
            cursor: 2,
            payload: DurablePayload::Record(step_record),
            lane: Some("main".into()),
            run_id: Some("run-1".into()),
            turn: Some(2),
            recovery_id: None,
            operation_intent: Some(OperationIntent::Run),
        };

        assert!(!durable_step.is_entry());
        assert!(durable_step.is_record());
        assert_eq!(durable_step.seq(), 2);
        assert_eq!(
            durable_step.project_agent_event(),
            Some(crate::events::AgentEvent::TurnStart { turn_number: 2 })
        );
    }

    #[test]
    fn harness_event_durable_filtering_and_polling() {
        let store = MemoryStore::new("test-session");
        let hub = HarnessEventHub::new(10);
        let mut sub = hub.subscribe(&store).unwrap();

        // Publish live events
        hub.publish_agent_event(crate::events::AgentEvent::AgentStart);
        hub.publish_streaming(Some(StreamingState {
            lane: "main".into(),
            run_id: Some("run-1".into()),
            assistant_text: "chunk".into(),
            reasoning: String::new(),
            tool_call_ids: Vec::new(),
        }));

        // Publish durable event
        let entry = Entry {
            id: "entry-1".into(),
            parent_id: None,
            seq: 1,
            lane: "main".into(),
            timestamp: 1000,
            message: AgentMessage::Assistant {
                content: Some("Done".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: SurfaceOperation::Append,
            terminate: false,
        };
        hub.publish_durable(
            DurablePayload::Entry(entry),
            Some("main".into()),
            Some("run-1".into()),
            None,
        );

        let durable_events = hub.poll_durable(&mut sub).unwrap();
        assert_eq!(durable_events.len(), 1);
        assert_eq!(durable_events[0].cursor, 3);
        assert!(durable_events[0].is_entry());
    }
}
