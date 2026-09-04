use std::collections::HashSet;

use super::agent::AgentHarness;
use super::reducer::Reducer;
use super::store::SessionStore;
use super::types::{ProvisionedEntry, QueueKind, ReduceError};
use super::{EffectsError, ProcedureError, Snapshot, Subscription};

use crate::AgentMessage;

// ---------------------------------------------------------------------------
// LaneHandle
// ---------------------------------------------------------------------------

/// A validated durable lane identity.
///
/// Created by [`SessionAgent::lane`] or [`SessionAgent::main_lane`] after
/// confirming that the lane exists in the session store.  A handle whose
/// backing lane is later removed becomes stale; the owning session rejects
/// future operations on it with [`ReduceError::InvalidLane`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaneHandle {
    name: String,
}

impl LaneHandle {
    /// Validate the name and return a handle.
    ///
    /// Rejects empty or whitespace-only names.
    pub fn new(name: String) -> Result<Self, ReduceError> {
        if name.trim().is_empty() {
            return Err(ReduceError::InvalidLane(name));
        }
        Ok(Self { name })
    }

    /// The lane name.
    fn name(&self) -> &str {
        &self.name
    }
}

// ---------------------------------------------------------------------------
// SessionAgent
// ---------------------------------------------------------------------------

/// Canonical session facade owning one [`AgentHarness`] and the execution
/// ports for one saved session.
///
/// Every accepted operation enters the harness, receives durable intent before
/// external effects, and resolves from the same reduced state used during
/// recovery.
pub struct SessionAgent<S: SessionStore> {
    harness: AgentHarness<S>,
    /// Lanes whose effects were committed to an external store (executor
    /// path); validate_lane accepts them without requiring a reduced entry.
    known_external_lanes: HashSet<String>,
}

impl<S: SessionStore> SessionAgent<S> {
    pub fn new(harness: AgentHarness<S>) -> Self {
        Self {
            harness,
            known_external_lanes: HashSet::new(),
        }
    }

    /// Access the inner harness directly for introspection and testing.
    pub fn harness(&self) -> &AgentHarness<S> {
        &self.harness
    }
}

// ---------------------------------------------------------------------------
// Lane resolution
// ---------------------------------------------------------------------------

impl<S: SessionStore> SessionAgent<S> {
    /// Resolve an existing lane by name.
    ///
    /// Returns [`ReduceError::InvalidLane`] if the lane is not present in the
    /// current reduced state or the known-external-lane set.
    pub fn lane(&self, name: &str) -> Result<LaneHandle, ReduceError> {
        if self.known_external_lanes.contains(name) {
            return LaneHandle::new(name.into());
        }
        let state = Reducer::reduce(self.harness.store())?;
        if !state.lanes.iter().any(|lane| lane.name == name) {
            return Err(ReduceError::InvalidLane(name.into()));
        }
        LaneHandle::new(name.into())
    }

    /// Return the persisted `main` lane, or create it on the first lookup.
    ///
    /// The harness default-creates a `main` lane implicitly when entries use
    /// the default lane field, so this method succeeds for a fresh store
    /// without an explicit create call.
    pub fn main_lane(&self) -> Result<LaneHandle, ReduceError> {
        self.lane("main")
    }
}

// ---------------------------------------------------------------------------
// Child-lane bootstrap
// ---------------------------------------------------------------------------

impl<S: SessionStore> SessionAgent<S> {
    /// Bootstrap a child lane that does not yet exist with an initial prompt.
    ///
    /// This is the canonical way to create a new lane through the facade.
    /// The prompt and operation are parked through
    /// [`AgentHarness::accept_prompt_on_lane`].  Then effects are driven
    /// globally (in FIFO order across all lanes) until the new lane
    /// materialises in the reduced state.  Driving globally rather than
    /// lane-scoped ensures older pending effects from other lanes commit
    /// before the new lane's effects, preserving global sequence ordering.
    ///
    /// When an effect executor is attached, the prompt is committed
    /// synchronously during `accept_prompt_on_lane`, so the lane is
    /// already visible and the handle is returned directly.
    ///
    /// Driving the remaining effects to completion is left to
    /// [`SessionAgent::drive_to_completion`].
    ///
    /// Returns [`ProcedureError::Invalid`] if the lane already exists.
    pub fn bootstrap_child_lane(
        &mut self,
        lane_name: &str,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<LaneHandle, ProcedureError> {
        // Reject if the lane already exists in the reduced state.
        if self.lane(lane_name).is_ok() {
            return Err(ProcedureError::Invalid(format!(
                "lane {lane_name} already exists"
            )));
        }
        // Park the prompt and operation on the new lane.
        self.harness
            .accept_prompt_on_lane(lane_name, run_id, prompt)?;
        // If the lane already materialised (e.g. executor committed
        // synchronously into the same store), return the handle directly.
        if self.lane(lane_name).is_ok() {
            return self
                .lane(lane_name)
                .map_err(|e| ProcedureError::Invalid(e.to_string()));
        }
        // If no pending actions remain at all (executor already committed
        // all effects to its own target store), track the lane so subsequent
        // operations succeed without requiring it in the harness store.
        if self.harness.peek_action().is_none() {
            self.known_external_lanes.insert(lane_name.to_string());
            return LaneHandle::new(lane_name.into())
                .map_err(|e| ProcedureError::Invalid(e.to_string()));
        }
        // Drive globally (FIFO order) so older pending actions from other
        // lanes commit before the child lane's actions, preserving global
        // sequence ordering.
        while self.harness.drive_one()? {
            if self.lane(lane_name).is_ok() {
                return self
                    .lane(lane_name)
                    .map_err(|e| ProcedureError::Invalid(e.to_string()));
            }
        }
        // All pending actions were driven but the lane never appeared.
        Err(ProcedureError::Invalid(format!(
            "failed to bootstrap lane {lane_name}: all pending actions consumed"
        )))
    }
}

// ---------------------------------------------------------------------------
// Observation
// ---------------------------------------------------------------------------

impl<S: SessionStore> SessionAgent<S> {
    /// Take a point-in-time snapshot of the session.
    pub fn snapshot(&self) -> Result<Snapshot, ReduceError> {
        self.harness.snapshot()
    }

    /// Subscribe to lane-scoped events.
    ///
    /// The returned [`Subscription`] carries the current snapshot as its
    /// baseline; polling yields events committed from now on for the given
    /// lane.
    pub fn watch(&self, lane: &LaneHandle) -> Result<Subscription, ReduceError> {
        self.harness.watch(lane.name())
    }

    /// Subscribe to every session event (all lanes).
    pub fn watch_session(&self) -> Result<Subscription, ReduceError> {
        self.harness.watch_session()
    }
}

// ---------------------------------------------------------------------------
// Accepted operations — all scoped to the given lane
// ---------------------------------------------------------------------------

impl<S: SessionStore> SessionAgent<S> {
    /// Verify that `lane` still exists in the current reduced state.
    fn validate_lane(&self, lane: &LaneHandle) -> Result<(), ReduceError> {
        if self.known_external_lanes.contains(lane.name()) {
            return Ok(());
        }
        let state = Reducer::reduce(self.harness.store())?;
        if !state.lanes.iter().any(|l| l.name == lane.name()) {
            return Err(ReduceError::InvalidLane(lane.name().to_string()));
        }
        Ok(())
    }

    /// Accept a user prompt on the given lane and return the allocated entry
    /// id.  The prompt is parked (not yet durable) until effects are driven.
    pub fn accept_prompt(
        &mut self,
        lane: &LaneHandle,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<String, ProcedureError> {
        self.validate_lane(lane)
            .map_err(|e| ProcedureError::Invalid(e.to_string()))?;
        self.harness
            .accept_prompt_on_lane(lane.name(), run_id, prompt)
    }

    /// Accept a user prompt and drive effects to completion, returning the canonical AcceptedRun token.
    pub fn accept_prompt_and_drive(
        &mut self,
        lane: &LaneHandle,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<crate::harness::AcceptedRun, ProcedureError> {
        self.validate_lane(lane)
            .map_err(|e| ProcedureError::Invalid(e.to_string()))?;
        self.harness
            .accept_prompt_and_drive_on_lane(lane.name(), run_id, prompt)
    }

    /// Validate an AcceptedRun token against this session's state and store.
    pub fn validate_accepted_run(
        &self,
        accepted: &crate::harness::AcceptedRun,
    ) -> Result<(), ReduceError> {
        self.harness.validate_accepted_run(accepted)
    }

    /// Enqueue a provisioning target on the lane.
    ///
    /// When `run_id` is `Some` the entry is bound to that run; `None`
    /// enqueues it unbound.
    pub fn enqueue(
        &mut self,
        lane: &LaneHandle,
        run_id: Option<&str>,
        queue: QueueKind,
        target: ProvisionedEntry,
    ) -> Result<(), ProcedureError> {
        self.validate_lane(lane)
            .map_err(|e| ProcedureError::Invalid(e.to_string()))?;
        match run_id {
            Some(id) => self.harness.enqueue_on_lane(lane.name(), id, queue, target),
            None => self
                .harness
                .enqueue_unbound_on_lane(lane.name(), queue, target),
        }
    }

    /// Cancel a queued entry on the lane.
    ///
    /// When `run_id` is `Some` the entry must be bound to that run; `None`
    /// matches an unbound entry.
    pub fn cancel_queued(
        &mut self,
        lane: &LaneHandle,
        run_id: Option<&str>,
        entry_id: &str,
    ) -> Result<(), ProcedureError> {
        self.validate_lane(lane)
            .map_err(|e| ProcedureError::Invalid(e.to_string()))?;
        match run_id {
            Some(id) => self
                .harness
                .cancel_queued_on_lane(lane.name(), id, entry_id),
            None => {
                let state = Reducer::reduce(self.harness.store())
                    .map_err(|e| ProcedureError::Invalid(format!("reduce failed: {e:?}")))?;
                let lane_state = state.lane(lane.name()).ok_or_else(|| {
                    ProcedureError::Invalid(format!("unknown lane: {}", lane.name()))
                })?;
                let queued = lane_state
                    .queued
                    .iter()
                    .find(|entry| entry.target.id == entry_id)
                    .ok_or_else(|| ProcedureError::Invalid("queued entry does not exist".into()))?;
                if queued.run_id.is_some() {
                    return Err(ProcedureError::Invalid(format!(
                        "queued entry {} is bound to run {:?}, not unbound",
                        entry_id, queued.run_id
                    )));
                }
                self.harness.cancel_unbound_on_lane(lane.name(), entry_id)
            }
        }
    }

    /// Consume a queued entry on the lane and record the consumption.
    ///
    /// When `run_id` is `Some` the entry must be bound to that run; `None`
    /// matches an unbound entry.
    pub fn consume_queued(
        &mut self,
        lane: &LaneHandle,
        run_id: Option<&str>,
        entry_id: &str,
    ) -> Result<(), ProcedureError> {
        self.validate_lane(lane)
            .map_err(|e| ProcedureError::Invalid(e.to_string()))?;
        match run_id {
            Some(id) => self
                .harness
                .consume_queued_on_lane(lane.name(), id, entry_id),
            None => {
                let state = Reducer::reduce(self.harness.store())
                    .map_err(|e| ProcedureError::Invalid(format!("reduce failed: {e:?}")))?;
                let lane_state = state.lane(lane.name()).ok_or_else(|| {
                    ProcedureError::Invalid(format!("unknown lane: {}", lane.name()))
                })?;
                let queued = lane_state
                    .queued
                    .iter()
                    .find(|entry| entry.target.id == entry_id)
                    .ok_or_else(|| ProcedureError::Invalid("queued entry does not exist".into()))?;
                if queued.run_id.is_some() {
                    return Err(ProcedureError::Invalid(format!(
                        "queued entry {} is bound to run {:?}, not unbound",
                        entry_id, queued.run_id
                    )));
                }
                self.harness.consume_unbound_on_lane(lane.name(), entry_id)
            }
        }
    }

    /// Request that the currently open operation on the lane be aborted.
    ///
    /// Abort intent is durable; reconciliation happens when the operation
    /// finishes.  The supplied lane must own `run_id` (the lane's reduced
    /// [`LaneState::open_operation`] must match).
    pub fn request_abort(&mut self, lane: &LaneHandle, run_id: &str) -> Result<(), ProcedureError> {
        let state = Reducer::reduce(self.harness.store())
            .map_err(|e| ProcedureError::Invalid(format!("reduce failed: {e:?}")))?;
        let lane_state = state
            .lanes
            .iter()
            .find(|l| l.name == lane.name())
            .ok_or_else(|| ProcedureError::Invalid(format!("unknown lane: {}", lane.name())))?;
        if lane_state.open_operation.as_deref() != Some(run_id) {
            return Err(ProcedureError::Invalid(format!(
                "lane {} does not own operation {}",
                lane.name(),
                run_id
            )));
        }
        self.harness.request_abort(run_id)
    }
}

// ---------------------------------------------------------------------------
// Effect driving — scoped to the selected lane
// ---------------------------------------------------------------------------

impl<S: SessionStore> SessionAgent<S> {
    /// Drive one parked effect on the lane.  Returns `true` if an action was
    /// executed, `false` if the lane is idle.
    pub fn drive_one(&mut self, lane: &LaneHandle) -> Result<bool, EffectsError> {
        self.validate_lane(lane).map_err(EffectsError::from)?;
        self.harness.drive_one_on_lane(lane.name())
    }

    /// Drive all pending effects on the lane to completion.
    pub fn drive_to_completion(&mut self, lane: &LaneHandle) -> Result<(), EffectsError> {
        self.validate_lane(lane).map_err(EffectsError::from)?;
        self.harness.drive_to_completion_on_lane(lane.name())
    }
}

// ---------------------------------------------------------------------------
// Recovery planning and execution
// ---------------------------------------------------------------------------

impl<S: SessionStore> SessionAgent<S> {
    /// Plan recovery for the given lane from the reduced session state.
    pub fn plan_recovery(&self, lane: &LaneHandle) -> Result<super::RecoveryPlan, ReduceError> {
        let state = Reducer::reduce(self.harness.store())?;
        let lane_state = state
            .lane(lane.name())
            .ok_or_else(|| ReduceError::InvalidLane(lane.name().to_string()))?;

        let open_operation_ids = lane_state
            .open_operation
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut safe_tools_to_replay = Vec::new();
        let mut unreplayable_tools = 0;

        for tool in &lane_state.tools {
            if !tool.completed {
                if let Some(record) = self.harness.store().records().iter().find(|r| {
                    if let super::Record::ToolStarted { tool_call_id, .. } = r {
                        tool_call_id == &tool.tool_call_id
                    } else {
                        false
                    }
                }) {
                    safe_tools_to_replay.push(record.clone());
                } else {
                    unreplayable_tools += 1;
                }
            }
        }

        let mut abort_requested_operation_ids = Vec::new();
        if lane_state.abort_requested {
            if let Some(ref op) = lane_state.open_operation {
                abort_requested_operation_ids.push(op.clone());
            }
        }

        let source_sequence = self
            .harness
            .store()
            .entries()
            .iter()
            .map(|e| e.seq)
            .chain(
                self.harness
                    .store()
                    .records()
                    .iter()
                    .map(super::Record::seq),
            )
            .max()
            .unwrap_or(0);

        let decision = if unreplayable_tools > 0 {
            super::RecoveryDecision::AbortUnsafeTool
        } else if !safe_tools_to_replay.is_empty() {
            super::RecoveryDecision::ReplaySafeToolsThenResume
        } else if lane_state.open_operation.is_some() {
            super::RecoveryDecision::ResumeFromLeaf
        } else {
            super::RecoveryDecision::None
        };
        Ok(super::RecoveryPlan {
            session_id: self.harness.store().session_id().to_string(),
            lane: lane.name().to_string(),
            source_sequence,
            decision,
            open_operation: lane_state.open_operation.clone(),
            interrupted_tools: Vec::new(),
            queued_work: lane_state
                .queued
                .iter()
                .map(|queued| super::QueuedWorkDiagnostic {
                    entry_id: queued.target.id.clone(),
                    queue: queued.queue.clone(),
                })
                .collect(),
            open_operation_ids,

            safe_tools_to_replay,
            unreplayable_tools,
            abort_requested_operation_ids,
        })
    }

    /// Execute a recovery plan on the session.
    pub fn execute_recovery(
        &mut self,
        plan: &super::RecoveryPlan,
    ) -> Result<super::RecoveryResult, ProcedureError> {
        let lane = LaneHandle::new(plan.lane.clone())
            .map_err(|e| ProcedureError::Invalid(e.to_string()))?;
        self.validate_lane(&lane)
            .map_err(|e| ProcedureError::Invalid(e.to_string()))?;

        let mut recovered_open_operations = 0;
        for op_id in &plan.open_operation_ids {
            if plan.abort_requested_operation_ids.contains(op_id) {
                let _ = self.harness.reconcile_abort_run(op_id);
            }
            recovered_open_operations += 1;
        }

        Ok(super::RecoveryResult {
            recovered_open_operations,
            open_operation_ids: plan.open_operation_ids.clone(),
            abort_requested_operation_ids: plan.abort_requested_operation_ids.clone(),
            unreplayable_tools: plan.unreplayable_tools,
            safe_tools_to_replay: plan.safe_tools_to_replay.clone(),
        })
    }
}
