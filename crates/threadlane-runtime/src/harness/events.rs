use super::store::SessionStore;
use super::types::{Entry, OperationIntent, Record, ReduceError, ReducedState};

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StreamingState {
    lane: String,
    run_id: Option<String>,
    assistant_text: String,
    reasoning: String,
    tool_call_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    session_id: String,
    pub state: ReducedState,
    pub entries: Vec<Entry>,
    pub records: Vec<Record>,
    #[serde(default)]
    pub(crate) streaming: Option<StreamingState>,
}

impl Snapshot {
    pub(crate) fn from_store<S: SessionStore>(store: &S) -> Result<Self, ReduceError> {
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
    use threadlane_protocol::AgentMessage;

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
                            images: Vec::new(),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventPayload {
    EntryCommitted(Entry),
    RecordCommitted(Record),
    Fault(String),
    Streaming(Option<StreamingState>),
    Agent(threadlane_protocol::AgentEvent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessEvent {
    id: u64,
    payload: EventPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery_id: Option<String>,
    /// The correlated [`OperationIntent`] for an [`EventPayload::RecordCommitted`]
    /// that wraps a [`Record::OperationFinished`]; resolved by the event hub from
    /// the matching [`Record::OperationStarted`].  `None` for other payloads and
    /// for finished operations whose start was not observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation_intent: Option<OperationIntent>,
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

    pub(crate) fn publish(&self, payload: EventPayload) -> HarnessEvent {
        self.publish_identified(payload, None, None, None)
    }

    pub(crate) fn publish_agent_event(&self, event: threadlane_protocol::AgentEvent) -> HarnessEvent {
        self.publish(EventPayload::Agent(event))
    }

    pub(crate) fn publish_identified(
        &self,
        payload: EventPayload,
        lane: Option<String>,
        run_id: Option<String>,
        recovery_id: Option<String>,
    ) -> HarnessEvent {
        self.publish_identified_with_turn(payload, lane, run_id, None, recovery_id)
    }

    pub(crate) fn publish_identified_with_turn(
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

    #[cfg(test)]
    fn publish_streaming(&self, state: Option<StreamingState>) -> HarnessEvent {
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

    pub(crate) fn subscribe<S: SessionStore>(
        &self,
        store: &S,
    ) -> Result<Subscription, ReduceError> {
        self.subscribe_for_lane(store, None)
    }

    pub(crate) fn subscribe_for_lane<S: SessionStore>(
        &self,
        store: &S,
        lane: Option<&str>,
    ) -> Result<Subscription, ReduceError> {
        // Capture the cursor before the expensive snapshot build and hold no
        // lock across it: cloning every entry/record plus a full reduce while
        // holding the hub lock blocked streaming publishers for O(n). Events
        // committed during the build are re-delivered by poll (at-least-once)
        // rather than skipped, so no published event is ever lost.
        let (next_id, streaming) = {
            let state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            (
                state.next_id,
                state
                    .streaming
                    .clone()
                    .filter(|stream| lane.is_none_or(|lane| stream.lane == lane)),
            )
        };
        let mut snapshot = Snapshot::from_store(store)?;
        snapshot.streaming = streaming;

        // Hydrate operation_intents from existing store records so a fresh
        // hub after restart can correlate a later OperationFinished with its
        // original OperationStarted intent.
        {
            let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            hydrate_intents_from_store(&mut state.operation_intents, store);
        }

        Ok(Subscription {
            snapshot,
            next_id,
            lane: lane.map(str::to_owned),
        })
    }

    fn poll(&self, subscription: &mut Subscription) -> Result<Vec<HarnessEvent>, EventError> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::MemoryStore;

    #[tokio::test]
    async fn subscription_waits_for_publication_without_polling() {
        let hub = HarnessEventHub::new(8);
        let store = MemoryStore::new("session");
        let mut subscription = hub.subscribe(&store).unwrap();
        let publisher = hub.clone();

        tokio::spawn(async move {
            tokio::task::yield_now().await;
            publisher.publish_agent_event(threadlane_protocol::AgentEvent::AgentStart);
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
}
