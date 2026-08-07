use super::store::SessionStore;
use super::types::{Entry, Record, ReduceError, ReducedState};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventPayload {
    EntryCommitted(Entry),
    RecordCommitted(Record),
    Fault(String),
    Streaming(Option<StreamingState>),
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
}

#[derive(Debug)]
struct HarnessEventHubState {
    capacity: usize,
    next_id: u64,
    events: VecDeque<HarnessEvent>,
    streaming: Option<StreamingState>,
}

impl HarnessEventHub {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HarnessEventHubState {
                capacity: capacity.max(1),
                next_id: 1,
                events: VecDeque::new(),
                streaming: None,
            })),
        }
    }

    pub fn publish(&self, payload: EventPayload) -> HarnessEvent {
        self.publish_identified(payload, None, None, None)
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
        let event = HarnessEvent {
            id: state.next_id,
            payload,
            lane,
            run_id,
            turn,
            recovery_id,
        };
        state.next_id += 1;
        if state.events.len() == state.capacity {
            state.events.pop_front();
        }
        state.events.push_back(event.clone());
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
        };
        hub.next_id += 1;
        if hub.events.len() == hub.capacity {
            hub.events.pop_front();
        }
        hub.events.push_back(event.clone());
        event
    }

    pub fn streaming_state(&self) -> Option<StreamingState> {
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
        let state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let mut snapshot = Snapshot::from_store(store)?;
        snapshot.streaming = state
            .streaming
            .clone()
            .filter(|stream| lane.is_none_or(|lane| stream.lane == lane));
        Ok(Subscription {
            snapshot,
            next_id: state.next_id,
            lane: lane.map(str::to_owned),
        })
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
            .last()
        {
            subscription.next_id = last_seen + 1;
        }
        Ok(events)
    }

    pub fn unsubscribe(self) {}
}
