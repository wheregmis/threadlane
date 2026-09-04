use super::events::{EventPayload, HarnessEventHub};
use super::store::SessionStore;
use super::telemetry::{ExecutionContext, NoopTelemetry, TelemetrySink};
use super::types::Entry;
use super::types::{Record, ReduceError};
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum EffectAction {
    AppendEntry { entry: Entry },
    AppendRecord { id: String, record: Record },
}

impl EffectAction {
    fn lane(&self) -> &str {
        match self {
            Self::AppendEntry { entry } => &entry.lane,
            Self::AppendRecord { record, .. } => record.lane(),
        }
    }

    pub(crate) fn id(&self) -> &str {
        match self {
            Self::AppendEntry { entry, .. } => &entry.id,
            Self::AppendRecord { id, .. } => id,
        }
    }

    fn seq(&self) -> u64 {
        match self {
            Self::AppendEntry { entry } => entry.seq,
            Self::AppendRecord { record, .. } => record.seq(),
        }
    }

    fn run_id(&self) -> Option<&str> {
        match self {
            Self::AppendEntry { .. } => None,
            Self::AppendRecord { record, .. } => record.run_id(),
        }
    }

    fn apply<S: SessionStore>(&self, store: &mut S) -> Result<(), ReduceError> {
        match self {
            Self::AppendEntry { entry, .. } => store.append_entry(entry.clone()),
            Self::AppendRecord { record, .. } => store.append_record(record.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectsError {
    Closed,
    Faulted(ReduceError),
    Empty,
    UnexpectedAction { expected: String, actual: String },
    Store(ReduceError),
}

impl std::fmt::Display for EffectsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for EffectsError {}

impl From<ReduceError> for EffectsError {
    fn from(error: ReduceError) -> Self {
        Self::Store(error)
    }
}

/// Deterministic action gate used by procedures and crash-prefix tests.
/// Parking is inert; only execute methods cross into the store.
pub struct GatedEffects {
    pending: VecDeque<EffectAction>,
    committed_sequences: Vec<u64>,
    closed: bool,
    fault: Option<ReduceError>,
    executor: Option<EffectExecutor>,
    telemetry: Arc<dyn TelemetrySink>,
}

type EffectExecutor = Box<dyn FnMut(EffectAction) -> Result<(), ReduceError> + Send + Sync>;

impl std::fmt::Debug for GatedEffects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatedEffects")
            .field("pending", &self.pending)
            .field("committed_sequences", &self.committed_sequences)
            .field("closed", &self.closed)
            .field("fault", &self.fault)
            .field("production", &self.executor.is_some())
            .finish()
    }
}

impl Default for GatedEffects {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            committed_sequences: Vec::new(),
            closed: false,
            fault: None,
            executor: None,
            telemetry: Arc::new(NoopTelemetry),
        }
    }
}

impl GatedEffects {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Uses the same procedure-facing effects surface for production commits.
    /// The executor owns the real store/effect adapters and is called only
    /// after procedure validation has accepted the action.
    pub(crate) fn with_executor(
        executor: impl FnMut(EffectAction) -> Result<(), ReduceError> + Send + Sync + 'static,
    ) -> Self {
        Self::with_executor_and_telemetry(executor, Arc::new(NoopTelemetry))
    }

    pub(crate) fn with_executor_and_telemetry(
        executor: impl FnMut(EffectAction) -> Result<(), ReduceError> + Send + Sync + 'static,
        telemetry: Arc<dyn TelemetrySink>,
    ) -> Self {
        Self {
            executor: Some(Box::new(executor)),
            telemetry,
            ..Self::default()
        }
    }

    pub(crate) fn with_telemetry(telemetry: Arc<dyn TelemetrySink>) -> Self {
        Self {
            telemetry,
            ..Self::default()
        }
    }

    fn notify_committed(&self, action: &EffectAction) {
        let mut context = ExecutionContext::default();
        context.lane = Some(action.lane().to_owned());
        context.run_id = action.run_id().map(str::to_owned);
        context.set_attribute(
            "effect",
            match action {
                EffectAction::AppendEntry { .. } => "append_entry",
                EffectAction::AppendRecord { .. } => "append_record",
            },
        );
        context.set_attribute("effect_id", action.id().to_owned());
        self.telemetry.event("effect_committed", &context);
    }

    pub(crate) fn park(&mut self, action: EffectAction) -> Result<(), EffectsError> {
        if let Some(error) = &self.fault {
            return Err(EffectsError::Faulted(error.clone()));
        }
        if self.closed {
            return Err(EffectsError::Closed);
        }
        if let Some(executor) = self.executor.as_mut() {
            let committed_action = action.clone();
            let seq = action.seq();
            if let Err(error) = executor(action) {
                self.fault = Some(error.clone());
                return Err(EffectsError::Faulted(error));
            }
            self.committed_sequences.push(seq);
            // The executor has committed the action before observers are told.
            // Keep telemetry aligned with the durable boundary.
            self.notify_committed(&committed_action);
        } else {
            self.pending.push_back(action);
        }
        Ok(())
    }

    pub(crate) fn peek_action(&self) -> Option<&EffectAction> {
        self.pending.front()
    }

    /// Return the oldest parked action for one lane without blocking on other lanes.
    pub(crate) fn peek_action_on_lane(&self, lane: &str) -> Option<&EffectAction> {
        self.pending.iter().find(|action| action.lane() == lane)
    }

    pub(crate) fn has_pending_on_lane(&self, lane: &str) -> bool {
        self.pending.iter().any(|action| action.lane() == lane)
    }

    pub(crate) fn pending_sequences(&self) -> impl Iterator<Item = u64> + '_ {
        self.pending
            .iter()
            .map(EffectAction::seq)
            .chain(self.committed_sequences.iter().copied())
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn has_executor(&self) -> bool {
        self.executor.is_some()
    }

    fn execute_pending<S: SessionStore>(
        &mut self,
        store: &mut S,
        lane: Option<&str>,
        id: &str,
    ) -> Result<EffectAction, EffectsError> {
        if let Some(error) = &self.fault {
            return Err(EffectsError::Faulted(error.clone()));
        }
        if self.closed {
            return Err(EffectsError::Closed);
        }
        let index = match lane {
            Some(lane) => self.pending.iter().position(|action| action.lane() == lane),
            None => (!self.pending.is_empty()).then_some(0),
        }
        .ok_or(EffectsError::Empty)?;
        let action = &self.pending[index];
        if action.id() != id {
            return Err(EffectsError::UnexpectedAction {
                expected: action.id().to_owned(),
                actual: id.to_owned(),
            });
        }
        if let Err(error) = action.apply(store) {
            self.fault = Some(error.clone());
            return Err(EffectsError::Faulted(error));
        }
        let action = self
            .pending
            .remove(index)
            .expect("action index was present");
        self.notify_committed(&action);
        Ok(action)
    }

    #[cfg(test)]
    fn execute_action<S: SessionStore>(
        &mut self,
        store: &mut S,
        id: &str,
    ) -> Result<EffectAction, EffectsError> {
        self.execute_pending(store, None, id)
    }

    fn execute_action_on_lane<S: SessionStore>(
        &mut self,
        store: &mut S,
        lane: &str,
        id: &str,
    ) -> Result<EffectAction, EffectsError> {
        self.execute_pending(store, Some(lane), id)
    }

    #[cfg(test)]
    pub(crate) fn run_to_completion<S: SessionStore>(
        &mut self,
        store: &mut S,
    ) -> Result<Vec<EffectAction>, EffectsError> {
        let mut completed = Vec::new();
        while let Some(id) = self.peek_action().map(|action| action.id().to_owned()) {
            completed.push(self.execute_action(store, &id)?);
        }
        Ok(completed)
    }

    /// Crosses the persistence gate once for the complete pending procedure.
    /// The queue is retained unchanged if the store rejects the durable unit.
    pub(crate) fn run_to_completion_atomically<S: SessionStore>(
        &mut self,
        store: &mut S,
    ) -> Result<Vec<EffectAction>, EffectsError> {
        if self.closed {
            return Err(EffectsError::Closed);
        }
        if let Some(error) = &self.fault {
            return Err(EffectsError::Faulted(error.clone()));
        }
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let actions = self.pending.iter().cloned().collect::<Vec<_>>();
        if let Err(error) = store.append_actions_atomically(&actions) {
            self.fault = Some(error.clone());
            return Err(EffectsError::Faulted(error));
        }
        self.pending.clear();
        for action in &actions {
            self.committed_sequences.push(action.seq());
            self.notify_committed(action);
        }
        Ok(actions)
    }

    pub fn run_to_completion_on_lane<S: SessionStore>(
        &mut self,
        store: &mut S,
        lane: &str,
    ) -> Result<Vec<EffectAction>, EffectsError> {
        let mut completed = Vec::new();
        while let Some(id) = self
            .peek_action_on_lane(lane)
            .map(|action| action.id().to_owned())
        {
            completed.push(self.execute_action_on_lane(store, lane, &id)?);
        }
        Ok(completed)
    }

    fn execute_with_events<S: SessionStore>(
        &mut self,
        store: &mut S,
        hub: &mut HarnessEventHub,
        lane: Option<&str>,
        id: &str,
    ) -> Result<EffectAction, EffectsError> {
        let action = match self.execute_pending(store, lane, id) {
            Ok(action) => action,
            Err(error) => {
                hub.publish(EventPayload::Fault(format!("{error:?}")));
                return Err(error);
            }
        };
        publish_committed(hub, &action);
        Ok(action)
    }

    pub(crate) fn execute_action_with_events<S: SessionStore>(
        &mut self,
        store: &mut S,
        hub: &mut HarnessEventHub,
        id: &str,
    ) -> Result<EffectAction, EffectsError> {
        self.execute_with_events(store, hub, None, id)
    }

    pub(crate) fn execute_action_on_lane_with_events<S: SessionStore>(
        &mut self,
        store: &mut S,
        hub: &mut HarnessEventHub,
        lane: &str,
        id: &str,
    ) -> Result<EffectAction, EffectsError> {
        self.execute_with_events(store, hub, Some(lane), id)
    }

    pub(crate) fn run_to_completion_with_events<S: SessionStore>(
        &mut self,
        store: &mut S,
        hub: &mut HarnessEventHub,
    ) -> Result<Vec<EffectAction>, EffectsError> {
        let mut completed = Vec::new();
        while let Some(id) = self.peek_action().map(|action| action.id().to_owned()) {
            completed.push(self.execute_action_with_events(store, hub, &id)?);
        }
        Ok(completed)
    }

    pub(crate) fn run_to_completion_on_lane_with_events<S: SessionStore>(
        &mut self,
        store: &mut S,
        hub: &mut HarnessEventHub,
        lane: &str,
    ) -> Result<Vec<EffectAction>, EffectsError> {
        let mut completed = Vec::new();
        while let Some(id) = self
            .peek_action_on_lane(lane)
            .map(|action| action.id().to_owned())
        {
            completed.push(self.execute_action_on_lane_with_events(store, hub, lane, &id)?);
        }
        Ok(completed)
    }

    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.pending.clear();
    }

    pub fn fault(&self) -> Option<&ReduceError> {
        self.fault.as_ref()
    }
}

fn publish_committed(hub: &HarnessEventHub, action: &EffectAction) {
    let payload = match action {
        EffectAction::AppendEntry { entry } => EventPayload::EntryCommitted(entry.clone()),
        EffectAction::AppendRecord { record, .. } => EventPayload::RecordCommitted(record.clone()),
    };
    if let EffectAction::AppendRecord { record, .. } = action {
        hub.publish_identified_with_turn(
            payload,
            Some(action.lane().to_owned()),
            record.run_id().map(str::to_owned),
            record.turn(),
            None,
        );
    } else {
        hub.publish_identified(payload, Some(action.lane().to_owned()), None, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::MemoryStore;
    use crate::types::AgentMessage;

    fn message(text: &str) -> AgentMessage {
        AgentMessage::user(text, Vec::new())
    }

    #[test]
    fn parked_actions_are_inert_and_execute_in_fifo_order() {
        let mut gate = GatedEffects::new();
        gate.park(EffectAction::AppendEntry {
            entry: Entry {
                id: "entry_1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: message("one"),
                surface_op: crate::harness::SurfaceOperation::Append,
                terminate: false,
            },
        })
        .unwrap();
        gate.park(EffectAction::AppendEntry {
            entry: Entry {
                id: "entry_2".into(),
                parent_id: Some("entry_1".into()),
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: message("two"),
                surface_op: crate::harness::SurfaceOperation::Append,
                terminate: false,
            },
        })
        .unwrap();
        let mut store = MemoryStore::new("s");
        assert_eq!(store.entries().len(), 0);
        assert_eq!(gate.peek_action().unwrap().id(), "entry_1");
        gate.run_to_completion(&mut store).unwrap();
        assert_eq!(store.entries().len(), 2);
    }

    #[test]
    fn close_rejects_new_work_without_mutating_store() {
        let mut gate = GatedEffects::new();
        gate.close();
        assert_eq!(
            gate.park(EffectAction::AppendEntry {
                entry: Entry {
                    id: "late".into(),
                    parent_id: None,
                    lane: "main".into(),
                    seq: 1,
                    timestamp: 1,
                    message: message("late"),
                    surface_op: crate::harness::SurfaceOperation::Append,
                    terminate: false,
                },
            }),
            Err(EffectsError::Closed)
        );
        let mut store = MemoryStore::new("s");
        assert_eq!(gate.run_to_completion(&mut store), Ok(Vec::new()));
        assert!(store.entries().is_empty());
    }

    #[test]
    fn failed_action_faults_the_gate_and_blocks_follow_up_work() {
        let mut gate = GatedEffects::new();
        gate.park(EffectAction::AppendEntry {
            entry: Entry {
                id: "entry-1".into(),
                parent_id: Some("missing".into()),
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: message("bad"),
                surface_op: crate::harness::SurfaceOperation::Append,
                terminate: false,
            },
        })
        .unwrap();
        let mut store = MemoryStore::new("s");
        assert!(matches!(
            gate.execute_action(&mut store, "entry-1"),
            Err(EffectsError::Faulted(ReduceError::MissingParent(_)))
        ));
        assert_eq!(gate.peek_action().map(EffectAction::id), Some("entry-1"));
        assert!(store.entries().is_empty());
        assert!(matches!(
            gate.park(EffectAction::AppendEntry {
                entry: Entry {
                    id: "entry-2".into(),
                    parent_id: None,
                    lane: "main".into(),
                    seq: 1,
                    timestamp: 1,
                    message: message("blocked"),
                    surface_op: crate::harness::SurfaceOperation::Append,
                    terminate: false,
                },
            }),
            Err(EffectsError::Faulted(ReduceError::MissingParent(_)))
        ));
    }

    #[test]
    fn injected_executor_commits_through_the_same_procedure_gate() {
        let store = std::sync::Arc::new(std::sync::Mutex::new(MemoryStore::new("session")));
        let target = store.clone();
        let mut effects =
            GatedEffects::with_executor(move |action| action.apply(&mut *target.lock().unwrap()));
        effects
            .park(EffectAction::AppendEntry {
                entry: Entry {
                    id: "entry-1".into(),
                    parent_id: None,
                    lane: "main".into(),
                    seq: 1,
                    timestamp: 1,
                    message: message("production"),
                    surface_op: crate::harness::SurfaceOperation::Append,
                    terminate: false,
                },
            })
            .unwrap();
        assert!(effects.peek_action().is_none());
        assert_eq!(store.lock().unwrap().entries().len(), 1);
    }

    #[test]
    fn lanes_can_progress_independently_while_preserving_lane_fifo() {
        let entry = |id: &str, lane: &str, seq: u64| Entry {
            id: id.into(),
            parent_id: None,
            lane: lane.into(),
            seq,
            timestamp: seq,
            message: message(id),
            surface_op: crate::harness::SurfaceOperation::Append,
            terminate: false,
        };
        let mut gate = GatedEffects::new();
        gate.park(EffectAction::AppendEntry {
            entry: entry("a1", "a", 2),
        })
        .unwrap();
        gate.park(EffectAction::AppendEntry {
            entry: entry("a2", "a", 3),
        })
        .unwrap();
        gate.park(EffectAction::AppendEntry {
            entry: entry("b1", "b", 1),
        })
        .unwrap();

        let mut store = MemoryStore::new("s");
        assert_eq!(gate.peek_action_on_lane("b").unwrap().id(), "b1");
        gate.execute_action_on_lane(&mut store, "b", "b1").unwrap();
        assert_eq!(gate.peek_action_on_lane("a").unwrap().id(), "a1");
        gate.execute_action_on_lane(&mut store, "a", "a1").unwrap();
        gate.execute_action_on_lane(&mut store, "a", "a2").unwrap();
        assert!(gate.peek_action_on_lane("a").is_none());
    }
}
