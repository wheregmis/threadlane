use super::{
    AbortProcedure, AssistantAttemptProcedure, CompactionProcedure, DeferredProcedure,
    DeferredResolution, EffectAction, EffectsError, GatedEffects, HarnessEventHub, HookRegistry,
    NavigationProcedure, NoToolRun, NoopTelemetry, OperationProcedure, ProcedureError,
    PromptProcedure, ProvisionedEntry, QueueKind, QueueProcedure, ReduceError, SessionStore,
    Snapshot, TelemetrySink, ToolBatchProcedure, ToolRecovery, ToolResult, ToolSpec,
};
use crate::types::{AgentMessage, TokenUsage};
use std::sync::Arc;

pub struct AgentHarness<S: SessionStore> {
    store: S,
    effects: GatedEffects,
    events: HarnessEventHub,
    hooks: HookRegistry,
    telemetry: Arc<dyn TelemetrySink>,
}

impl<S: SessionStore> AgentHarness<S> {
    pub fn new(store: S) -> Self {
        Self::with_events(store, HarnessEventHub::new(256))
    }

    pub(crate) fn with_events(store: S, events: HarnessEventHub) -> Self {
        Self {
            store,
            effects: GatedEffects::new(),
            events,
            hooks: HookRegistry::default(),
            telemetry: Arc::new(NoopTelemetry),
        }
    }

    pub fn with_events_and_hooks(store: S, events: HarnessEventHub, hooks: HookRegistry) -> Self {
        Self {
            store,
            effects: GatedEffects::new(),
            events,
            hooks,
            telemetry: Arc::new(NoopTelemetry),
        }
    }

    pub fn with_executor(
        store: S,
        events: HarnessEventHub,
        executor: impl FnMut(EffectAction) -> Result<(), ReduceError> + Send + Sync + 'static,
    ) -> Self {
        Self::with_executor_and_hooks(store, events, executor, HookRegistry::default())
    }

    pub fn with_executor_and_hooks(
        store: S,
        events: HarnessEventHub,
        executor: impl FnMut(EffectAction) -> Result<(), ReduceError> + Send + Sync + 'static,
        hooks: HookRegistry,
    ) -> Self {
        Self {
            store,
            effects: GatedEffects::with_executor(executor),
            events,
            hooks,
            telemetry: Arc::new(NoopTelemetry),
        }
    }

    pub fn with_telemetry(
        store: S,
        events: HarnessEventHub,
        telemetry: Arc<dyn TelemetrySink>,
    ) -> Self {
        Self {
            store,
            effects: GatedEffects::with_telemetry(telemetry.clone()),
            events,
            hooks: HookRegistry::default(),
            telemetry,
        }
    }

    pub fn with_executor_and_telemetry(
        store: S,
        events: HarnessEventHub,
        executor: impl FnMut(EffectAction) -> Result<(), ReduceError> + Send + Sync + 'static,
        telemetry: Arc<dyn TelemetrySink>,
    ) -> Self {
        Self {
            store,
            effects: GatedEffects::with_executor_and_telemetry(executor, telemetry.clone()),
            events,
            hooks: HookRegistry::default(),
            telemetry,
        }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    pub fn effects(&self) -> &GatedEffects {
        &self.effects
    }

    pub fn hooks(&self) -> &HookRegistry {
        &self.hooks
    }

    pub fn hooks_mut(&mut self) -> &mut HookRegistry {
        &mut self.hooks
    }

    pub fn telemetry(&self) -> &dyn TelemetrySink {
        self.telemetry.as_ref()
    }

    pub fn fault(&self) -> Option<&ReduceError> {
        self.effects.fault()
    }

    pub fn close(&mut self) {
        self.effects.close();
    }

    pub fn accept_no_tool_run(
        &mut self,
        run_id: &str,
        prompt: &str,
        assistant: AgentMessage,
    ) -> Result<(), super::ProcedureError> {
        NoToolRun::accept(&self.store, run_id, prompt, assistant, &mut self.effects)
    }

    pub fn accept_no_tool_run_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        prompt: &str,
        assistant: AgentMessage,
    ) -> Result<(), super::ProcedureError> {
        NoToolRun::accept_on_lane(
            &self.store,
            lane,
            run_id,
            prompt,
            assistant,
            &mut self.effects,
        )
    }

    pub fn resume_no_tool_run(
        &mut self,
        run_id: &str,
        prompt: &str,
        assistant: AgentMessage,
    ) -> Result<(), ProcedureError> {
        NoToolRun::resume(&self.store, run_id, prompt, assistant, &mut self.effects)
    }

    pub fn resume_no_tool_run_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        prompt: &str,
        assistant: AgentMessage,
    ) -> Result<(), ProcedureError> {
        NoToolRun::resume_on_lane(
            &self.store,
            lane,
            run_id,
            prompt,
            assistant,
            &mut self.effects,
        )
    }

    pub fn finish_assistant_attempt(
        &mut self,
        run_id: &str,
        result_entry_id: &str,
        usage: TokenUsage,
    ) -> Result<(), ProcedureError> {
        AssistantAttemptProcedure::finish(
            &self.store,
            run_id,
            result_entry_id,
            usage,
            &mut self.effects,
        )
    }

    pub fn record_provider_usage(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<u32, ProcedureError> {
        AssistantAttemptProcedure::record_usage(&self.store, run_id, usage, &mut self.effects)
    }

    pub fn record_discarded_usage(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), ProcedureError> {
        AssistantAttemptProcedure::record_discarded_usage(
            &self.store,
            run_id,
            usage,
            &mut self.effects,
        )
    }

    pub fn record_usage_adjustment(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), ProcedureError> {
        AssistantAttemptProcedure::record_adjustment(&self.store, run_id, usage, &mut self.effects)
    }

    pub fn schedule_retry(
        &mut self,
        run_id: &str,
        reason: &str,
        policy: super::RetryPolicy,
    ) -> Result<u32, ProcedureError> {
        super::RetryProcedure::schedule(&self.store, run_id, reason, policy, &mut self.effects)
    }

    pub fn begin_retry(&mut self, run_id: &str) -> Result<u32, ProcedureError> {
        super::RetryProcedure::begin(&self.store, run_id, &mut self.effects)
    }

    pub fn finish_operation(
        &mut self,
        run_id: &str,
        outcome: super::OperationOutcome,
        error: Option<String>,
    ) -> Result<(), ProcedureError> {
        OperationProcedure::finish(&self.store, run_id, outcome, error, &mut self.effects)
    }

    pub fn start_operation(
        &mut self,
        run_id: &str,
        source_leaf_id: Option<String>,
        intent: super::OperationIntent,
    ) -> Result<(), ProcedureError> {
        OperationProcedure::start(
            &self.store,
            run_id,
            source_leaf_id,
            intent,
            &mut self.effects,
        )
    }

    pub fn start_operation_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        source_leaf_id: Option<String>,
        intent: super::OperationIntent,
    ) -> Result<(), ProcedureError> {
        OperationProcedure::start_on_lane(
            &self.store,
            lane,
            run_id,
            source_leaf_id,
            intent,
            &mut self.effects,
        )
    }

    pub fn accept_prompt(
        &mut self,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<String, ProcedureError> {
        PromptProcedure::accept(&self.store, run_id, prompt, &mut self.effects)
    }

    pub fn accept_prompt_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<String, ProcedureError> {
        PromptProcedure::accept_on_lane(&self.store, lane, run_id, prompt, &mut self.effects)
    }

    #[cfg(test)]
    fn accept_prompt_and_drive(
        &mut self,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<super::AcceptedRun, ProcedureError> {
        self.accept_prompt_and_drive_on_lane("main", run_id, prompt)
    }

    pub fn accept_prompt_and_drive_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<super::AcceptedRun, ProcedureError> {
        let assistant_entry_id = self.accept_prompt_on_lane(lane, run_id, prompt)?;
        self.drive_to_completion_on_lane(lane)?;
        let accepted_through_seq = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.store.records().iter().map(|record| record.seq()))
            .max()
            .unwrap_or(0);
        let prompt_entry_id = format!("entry-{run_id}-user");
        Ok(super::AcceptedRun {
            session_id: self.store.session_id().to_string(),
            run_id: run_id.to_string(),
            lane: lane.to_string(),
            prompt_entry_id,
            assistant_entry_id,
            accepted_through_seq,
        })
    }

    pub fn validate_accepted_run(&self, accepted: &super::AcceptedRun) -> Result<(), ReduceError> {
        let session_id = self.store.session_id();
        if !session_id.is_empty()
            && !accepted.session_id.is_empty()
            && accepted.session_id != session_id
        {
            return Err(ReduceError::InvalidLane(format!(
                "accepted run session mismatch: expected {session_id}, got {}",
                accepted.session_id
            )));
        }
        if accepted.run_id.trim().is_empty() || accepted.lane.trim().is_empty() {
            return Err(ReduceError::InvalidLane(
                "accepted run id or lane is empty".into(),
            ));
        }
        if accepted.prompt_entry_id.trim().is_empty()
            || accepted.assistant_entry_id.trim().is_empty()
        {
            return Err(ReduceError::InvalidLane(
                "accepted prompt or assistant entry id is empty".into(),
            ));
        }
        if accepted.accepted_through_seq == 0 {
            return Err(ReduceError::InvalidLane(
                "accepted_through_seq must be > 0".into(),
            ));
        }
        let state = super::Reducer::reduce(&self.store)?;
        let lane = state.lane(&accepted.lane).ok_or_else(|| {
            ReduceError::InvalidLane(format!("lane {} not found in reduced state", accepted.lane))
        })?;
        if lane.open_operation.as_deref() != Some(&accepted.run_id) {
            return Err(ReduceError::InvalidLane(format!(
                "lane {} open operation is {:?}, expected {}",
                accepted.lane, lane.open_operation, accepted.run_id
            )));
        }
        self.validate_acceptance_proof(
            &accepted.run_id,
            &accepted.lane,
            &accepted.prompt_entry_id,
            &accepted.assistant_entry_id,
            accepted.accepted_through_seq,
        )
    }

    /// Validates the public, compact accepted-run token against committed state.
    /// The named prompt must be in the committed prefix and the operation must
    /// still be open on the same lane.
    pub(crate) fn validate_accepted_run_token(
        &self,
        run_id: &str,
        lane_name: &str,
        accepted_through_seq: u64,
    ) -> Result<(), ReduceError> {
        if run_id.is_empty() || lane_name.is_empty() || accepted_through_seq == 0 {
            return Err(ReduceError::InvalidLane(
                "accepted run token contains an empty identifier or zero prefix".into(),
            ));
        }
        let max_seq = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.store.records().iter().map(|record| record.seq()))
            .max()
            .unwrap_or(0);
        if accepted_through_seq > max_seq {
            return Err(ReduceError::InvalidLane(format!(
                "accepted prefix {accepted_through_seq} exceeds committed sequence {max_seq}"
            )));
        }
        let state = super::Reducer::reduce(&self.store)?;
        let lane = state.lane(lane_name).ok_or_else(|| {
            ReduceError::InvalidLane(format!("lane {lane_name} not found in reduced state"))
        })?;
        if lane.open_operation.as_deref() != Some(run_id) {
            return Err(ReduceError::InvalidLane(format!(
                "lane {lane_name} does not have accepted run {run_id} open"
            )));
        }
        let prompt_id = format!("entry-{run_id}-user");
        let assistant_id = format!("entry-{run_id}-assistant-1");
        self.validate_acceptance_proof(
            run_id,
            lane_name,
            &prompt_id,
            &assistant_id,
            accepted_through_seq,
        )
    }

    fn validate_acceptance_proof(
        &self,
        run_id: &str,
        lane_name: &str,
        prompt_id: &str,
        assistant_id: &str,
        accepted_through_seq: u64,
    ) -> Result<(), ReduceError> {
        let durable_max = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.store.records().iter().map(|record| record.seq()))
            .max()
            .unwrap_or(0);
        if accepted_through_seq > durable_max {
            return Err(ReduceError::InvalidLane(format!(
                "accepted prefix {accepted_through_seq} exceeds committed sequence {durable_max}"
            )));
        }
        let source_leaf = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                super::Record::OperationStarted {
                    id,
                    seq,
                    lane,
                    source_leaf_id,
                    intent: super::OperationIntent::Run,
                    ..
                } if id == run_id && lane == lane_name && *seq <= accepted_through_seq => {
                    source_leaf_id.clone().into()
                }
                _ => None,
            })
            .ok_or_else(|| {
                ReduceError::InvalidLane(format!(
            "accepted run {run_id} has no matching start in committed prefix {accepted_through_seq}"
        ))
            })?;
        let prompt = self
            .store
            .entries()
            .iter()
            .find(|entry| {
                entry.id == prompt_id
                    && entry.lane == lane_name
                    && entry.seq <= accepted_through_seq
            })
            .ok_or_else(|| {
                ReduceError::InvalidLane(format!(
            "accepted prompt {prompt_id} is absent from committed prefix {accepted_through_seq}"
        ))
            })?;
        if !prompt.message.is_user() || prompt.parent_id != source_leaf {
            return Err(ReduceError::InvalidLane(format!(
                "accepted prompt {prompt_id} does not match run {run_id} ancestry or role"
            )));
        }
        let has_assistant_reservation = self.store.records().iter().any(|record| matches!(record,
            super::Record::StepAttempt { seq, lane, run_id: proof_run, attempt: 1, result_entry_id, .. }
                if lane == lane_name && proof_run == run_id && result_entry_id == assistant_id
                    && *seq <= accepted_through_seq
        ));
        if !has_assistant_reservation {
            return Err(ReduceError::InvalidLane(format!(
                "assistant reservation {assistant_id} is absent from accepted run {run_id} prefix {accepted_through_seq}"
            )));
        }
        Ok(())
    }

    pub fn append_entry_gated(&mut self, entry: super::Entry) -> Result<(), ProcedureError> {
        let next = self
            .effects
            .pending_sequences()
            .max()
            .map_or(self.store.next_sequence(), |seq| seq + 1)
            .max(self.store.next_sequence());
        let mut entry = entry;
        if entry.seq < next {
            entry.seq = next;
        }
        self.effects
            .park(EffectAction::AppendEntry { entry })
            .map_err(ProcedureError::from)
    }

    pub fn append_record_gated(&mut self, record: super::Record) -> Result<(), ProcedureError> {
        let next = self
            .effects
            .pending_sequences()
            .max()
            .map_or(self.store.next_sequence(), |seq| seq + 1)
            .max(self.store.next_sequence());
        let record = if record.seq() < next {
            record.with_seq(next)
        } else {
            record
        };
        self.effects
            .park(EffectAction::AppendRecord {
                id: record.id().to_owned(),
                record,
            })
            .map_err(ProcedureError::from)
    }

    pub fn accept_compaction(
        &mut self,
        run_id: &str,
        summary: &str,
        context_snapshot_index: &[serde_json::Value],
    ) -> Result<(), ProcedureError> {
        CompactionProcedure::accept(
            &self.store,
            run_id,
            summary,
            context_snapshot_index,
            &mut self.effects,
        )
    }

    /// Checkpoints canonical context while preserving the caller's open run.
    pub fn checkpoint_open_run_compaction(
        &mut self,
        lane: &str,
        run_id: &str,
        summary: &str,
        context_snapshot_index: &[serde_json::Value],
        reason: super::CompactionReason,
    ) -> Result<(), ProcedureError> {
        CompactionProcedure::checkpoint_open_run(
            &self.store,
            lane,
            run_id,
            summary,
            context_snapshot_index,
            reason,
            &mut self.effects,
        )
    }

    pub fn drive_to_completion_atomically(&mut self) -> Result<(), ProcedureError> {
        self.effects
            .run_to_completion_atomically(&mut self.store)
            .map(|_| ())
            .map_err(ProcedureError::Effects)
    }

    pub fn accept_compaction_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        summary: &str,
    ) -> Result<(), ProcedureError> {
        CompactionProcedure::accept_on_lane(
            &self.store,
            lane,
            run_id,
            summary,
            &[],
            &mut self.effects,
        )
    }

    pub fn accept_navigation(
        &mut self,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
    ) -> Result<(), ProcedureError> {
        NavigationProcedure::accept(
            &self.store,
            run_id,
            target_leaf_id,
            summary,
            &mut self.effects,
        )
    }

    pub fn accept_navigation_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
    ) -> Result<(), ProcedureError> {
        NavigationProcedure::accept_on_lane(
            &self.store,
            lane,
            run_id,
            target_leaf_id,
            summary,
            &mut self.effects,
        )
    }

    pub fn resume_navigation(
        &mut self,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
    ) -> Result<(), ProcedureError> {
        NavigationProcedure::resume(
            &self.store,
            run_id,
            target_leaf_id,
            summary,
            &mut self.effects,
        )
    }

    pub fn resume_navigation_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
    ) -> Result<(), ProcedureError> {
        NavigationProcedure::resume_on_lane(
            &self.store,
            lane,
            run_id,
            target_leaf_id,
            summary,
            &mut self.effects,
        )
    }

    pub fn request_abort(&mut self, run_id: &str) -> Result<(), ProcedureError> {
        AbortProcedure::request(&self.store, run_id, &mut self.effects)
    }

    pub fn reconcile_abort(
        &mut self,
        run_id: &str,
        assistant_entry_id: &str,
    ) -> Result<(), ProcedureError> {
        AbortProcedure::reconcile(&self.store, run_id, assistant_entry_id, &mut self.effects)
    }

    pub fn reconcile_abort_run(&mut self, run_id: &str) -> Result<(), ProcedureError> {
        let result_entry_id = self
            .store
            .records()
            .iter()
            .rev()
            .find_map(|record| match record {
                super::Record::StepAttempt {
                    run_id: record_run_id,
                    result_entry_id,
                    ..
                } if record_run_id == run_id => Some(result_entry_id.clone()),
                _ => None,
            })
            .ok_or_else(|| ProcedureError::Invalid("operation has no provisioned result".into()))?;
        self.reconcile_abort(run_id, &result_entry_id)
    }

    pub fn enqueue(
        &mut self,
        run_id: &str,
        queue: QueueKind,
        target: ProvisionedEntry,
    ) -> Result<(), ProcedureError> {
        QueueProcedure::enqueue(&self.store, run_id, queue, target, &mut self.effects)
    }

    pub fn enqueue_unbound(
        &mut self,
        queue: QueueKind,
        target: ProvisionedEntry,
    ) -> Result<(), ProcedureError> {
        QueueProcedure::enqueue_unbound(&self.store, queue, target, &mut self.effects)
    }

    pub fn enqueue_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        queue: QueueKind,
        target: ProvisionedEntry,
    ) -> Result<(), ProcedureError> {
        QueueProcedure::enqueue_on_lane(&self.store, lane, run_id, queue, target, &mut self.effects)
    }

    pub fn enqueue_unbound_on_lane(
        &mut self,
        lane: &str,
        queue: QueueKind,
        target: ProvisionedEntry,
    ) -> Result<(), ProcedureError> {
        QueueProcedure::enqueue_unbound_on_lane(&self.store, lane, queue, target, &mut self.effects)
    }

    pub fn cancel_queued(&mut self, run_id: &str, entry_id: &str) -> Result<(), ProcedureError> {
        QueueProcedure::cancel(&self.store, run_id, entry_id, &mut self.effects)
    }

    pub fn cancel_queued_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        entry_id: &str,
    ) -> Result<(), ProcedureError> {
        QueueProcedure::cancel_on_lane(&self.store, lane, run_id, entry_id, &mut self.effects)
    }

    pub fn cancel_unbound(&mut self, entry_id: &str) -> Result<(), ProcedureError> {
        QueueProcedure::cancel_unbound(&self.store, entry_id, &mut self.effects)
    }

    pub fn cancel_unbound_on_lane(
        &mut self,
        lane: &str,
        entry_id: &str,
    ) -> Result<(), ProcedureError> {
        QueueProcedure::cancel_unbound_on_lane(&self.store, lane, entry_id, &mut self.effects)
    }

    pub fn consume_queued(&mut self, run_id: &str, entry_id: &str) -> Result<(), ProcedureError> {
        QueueProcedure::consume(&self.store, run_id, entry_id, &mut self.effects)
    }

    pub fn consume_queued_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        entry_id: &str,
    ) -> Result<(), ProcedureError> {
        QueueProcedure::consume_on_lane(&self.store, lane, run_id, entry_id, &mut self.effects)
    }

    pub fn consume_unbound(&mut self, entry_id: &str) -> Result<(), ProcedureError> {
        QueueProcedure::consume_unbound(&self.store, entry_id, &mut self.effects)
    }

    pub fn consume_unbound_on_lane(
        &mut self,
        lane: &str,
        entry_id: &str,
    ) -> Result<(), ProcedureError> {
        QueueProcedure::consume_unbound_on_lane(&self.store, lane, entry_id, &mut self.effects)
    }

    pub fn suspend_deferred(
        &mut self,
        run_id: &str,
        entry: super::Entry,
    ) -> Result<(), ProcedureError> {
        DeferredProcedure::suspend(&self.store, run_id, entry, &mut self.effects)
    }

    pub fn enqueue_deferred(
        &mut self,
        run_id: &str,
        target: ProvisionedEntry,
    ) -> Result<(), ProcedureError> {
        DeferredProcedure::enqueue(&self.store, run_id, target, &mut self.effects)
    }

    pub fn apply_deferred(&mut self, run_id: &str) -> Result<(), ProcedureError> {
        DeferredProcedure::apply_pending(&self.store, run_id, &mut self.effects)
    }

    pub fn set_fact(
        &mut self,
        lane: &str,
        key: impl Into<String>,
        value: impl Into<String>,
        run_id: Option<String>,
    ) -> Result<(), ProcedureError> {
        let key = key.into();
        if lane.trim().is_empty() || key.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "fact lane and key must be non-empty".into(),
            ));
        }
        let seq = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.store.records().iter().map(super::Record::seq))
            .chain(self.effects.pending_sequences())
            .max()
            .unwrap_or(0)
            + 1;
        self.effects
            .park(EffectAction::AppendRecord {
                id: format!("fact-{lane}-{key}-{seq}"),
                record: super::Record::FactSet {
                    id: format!("fact-record-{lane}-{key}-{seq}"),
                    seq,
                    lane: lane.into(),
                    timestamp: seq,
                    run_id,
                    key,
                    value: value.into(),
                },
            })
            .map_err(ProcedureError::from)
    }

    pub fn set_hook_resume_data(
        &mut self,
        lane: &str,
        hook_id: impl Into<String>,
        data: impl Into<String>,
        run_id: Option<String>,
    ) -> Result<(), ProcedureError> {
        let hook_id = hook_id.into();
        if lane.trim().is_empty() || hook_id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "hook lane and id must be non-empty".into(),
            ));
        }
        let seq = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.store.records().iter().map(super::Record::seq))
            .chain(self.effects.pending_sequences())
            .max()
            .unwrap_or(0)
            + 1;
        self.effects
            .park(EffectAction::AppendRecord {
                id: format!("hook-resume-data-action-{lane}-{hook_id}-{seq}"),
                record: super::Record::HookResumeData {
                    id: format!("hook-resume-data-{lane}-{hook_id}-{seq}"),
                    seq,
                    lane: lane.into(),
                    timestamp: seq,
                    run_id,
                    hook_id,
                    data: data.into(),
                },
            })
            .map_err(ProcedureError::from)
    }

    pub fn restore_hooks_for_lane(&mut self, lane: &str) -> Result<(), ReduceError> {
        let state = super::Reducer::reduce(&self.store)?;
        let data = state
            .lane(lane)
            .map(|lane| lane.resume_data.clone())
            .unwrap_or_default();
        self.hooks.restore_resume_data(&data);
        Ok(())
    }

    pub fn redeem_deferred(
        &mut self,
        run_id: &str,
        resolution: DeferredResolution,
    ) -> Result<bool, ProcedureError> {
        DeferredProcedure::redeem(&self.store, run_id, resolution, &mut self.effects)
    }

    pub fn start_tool_batch(
        &mut self,
        run_id: &str,
        assistant_entry_id: &str,
        specs: &[ToolSpec],
    ) -> Result<(), super::ProcedureError> {
        ToolBatchProcedure::start(
            &self.store,
            run_id,
            assistant_entry_id,
            specs,
            &mut self.effects,
        )
    }

    pub fn finish_tool(&mut self, run_id: &str, result: ToolResult) -> Result<(), ProcedureError> {
        ToolBatchProcedure::finish(&self.store, run_id, result, &mut self.effects)
    }

    pub fn finish_tool_with_usage(
        &mut self,
        run_id: &str,
        result: ToolResult,
        usage: TokenUsage,
    ) -> Result<(), ProcedureError> {
        ToolBatchProcedure::finish_with_usage(&self.store, run_id, result, usage, &mut self.effects)
    }

    pub fn finish_tool_batch(
        &mut self,
        run_id: &str,
        results: &[ToolResult],
        usage: TokenUsage,
    ) -> Result<(), ProcedureError> {
        ToolBatchProcedure::finish_batch(&self.store, run_id, results, usage, &mut self.effects)
    }

    pub fn finish_existing_tool(
        &mut self,
        run_id: &str,
        result: ToolResult,
    ) -> Result<(), ProcedureError> {
        ToolBatchProcedure::finish_existing(&self.store, run_id, result, None, &mut self.effects)
    }

    pub fn finish_existing_tool_batch(
        &mut self,
        run_id: &str,
        results: &[ToolResult],
        usage: TokenUsage,
    ) -> Result<(), ProcedureError> {
        ToolBatchProcedure::finish_existing_batch(
            &self.store,
            run_id,
            results,
            usage,
            &mut self.effects,
        )
    }

    pub fn resume_tool_batch(
        &mut self,
        run_id: &str,
        assistant_entry_id: &str,
        current_specs: &[ToolSpec],
    ) -> Result<Vec<ToolRecovery>, ProcedureError> {
        ToolBatchProcedure::resume(
            &self.store,
            run_id,
            assistant_entry_id,
            current_specs,
            &mut self.effects,
        )
    }

    pub fn peek_action(&self) -> Option<&EffectAction> {
        self.effects.peek_action()
    }

    pub fn peek_action_on_lane(&self, lane: &str) -> Option<&EffectAction> {
        self.effects.peek_action_on_lane(lane)
    }

    pub(crate) fn drive_one_on_lane(&mut self, lane: &str) -> Result<bool, EffectsError> {
        let Some(id) = self
            .effects
            .peek_action_on_lane(lane)
            .map(|action| action.id().to_owned())
        else {
            return Ok(false);
        };
        self.effects.execute_action_on_lane_with_events(
            &mut self.store,
            &mut self.events,
            lane,
            &id,
        )?;
        self.store.refresh().map_err(EffectsError::Store)?;
        Ok(true)
    }

    pub fn drive_one(&mut self) -> Result<bool, EffectsError> {
        let Some(id) = self
            .effects
            .peek_action()
            .map(|action| action.id().to_owned())
        else {
            return Ok(false);
        };
        self.effects
            .execute_action_with_events(&mut self.store, &mut self.events, &id)?;
        self.store.refresh().map_err(EffectsError::Store)?;
        Ok(true)
    }

    pub fn drive_to_completion(&mut self) -> Result<(), EffectsError> {
        self.effects
            .run_to_completion_with_events(&mut self.store, &mut self.events)?;
        if self.effects.has_executor() {
            self.store.refresh().map_err(EffectsError::Store)?;
        }
        Ok(())
    }

    pub(crate) fn drive_to_completion_on_lane(&mut self, lane: &str) -> Result<(), EffectsError> {
        self.effects.run_to_completion_on_lane_with_events(
            &mut self.store,
            &mut self.events,
            lane,
        )?;
        if self.effects.has_executor() {
            self.store.refresh().map_err(EffectsError::Store)?;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<Snapshot, ReduceError> {
        let mut snapshot = Snapshot::from_store(&self.store)?;
        snapshot.streaming = self.events.streaming_state();
        Ok(snapshot)
    }

    pub(crate) fn events(&self) -> &HarnessEventHub {
        &self.events
    }

    pub fn events_mut(&mut self) -> &mut HarnessEventHub {
        &mut self.events
    }

    pub fn subscribe(&self) -> Result<super::Subscription, ReduceError> {
        self.events.subscribe(&self.store)
    }

    pub fn watch_session(&self) -> Result<super::Subscription, ReduceError> {
        self.events.subscribe(&self.store)
    }

    pub fn watch(&self, lane: &str) -> Result<super::Subscription, ReduceError> {
        if !super::Reducer::reduce(&self.store)?
            .lanes
            .iter()
            .any(|state| state.name == lane)
        {
            return Err(ReduceError::InvalidLane(lane.into()));
        }
        self.events.subscribe_for_lane(&self.store, Some(lane))
    }
}

impl<S: SessionStore> SessionStore for AgentHarness<S> {
    fn session_id(&self) -> &str {
        self.store.session_id()
    }

    fn reduced_state(&self) -> Option<super::ReducedState> {
        self.store.reduced_state()
    }

    fn next_sequence(&self) -> u64 {
        self.store.next_sequence()
    }

    fn refresh(&mut self) -> Result<(), super::ReduceError> {
        self.store.refresh()
    }

    fn facts(&self) -> std::collections::BTreeMap<String, String> {
        self.store.facts()
    }

    fn entries(&self) -> &[super::Entry] {
        self.store.entries()
    }

    fn records(&self) -> &[super::Record] {
        self.store.records()
    }

    fn append_entry(&mut self, entry: super::Entry) -> Result<(), super::ReduceError> {
        let lane = entry.lane.clone();
        self.store.append_entry(entry.clone())?;
        self.events.publish_identified(
            super::EventPayload::EntryCommitted(entry),
            Some(lane),
            None,
            None,
        );
        Ok(())
    }

    fn append_record(&mut self, record: super::Record) -> Result<(), super::ReduceError> {
        let lane = record.lane().to_owned();
        let run_id = record.run_id().map(str::to_owned);
        let turn = record.turn();
        self.store.append_record(record.clone())?;
        self.events.publish_identified_with_turn(
            super::EventPayload::RecordCommitted(record),
            Some(lane),
            run_id,
            turn,
            None,
        );
        Ok(())
    }
}

#[cfg(test)]
mod accepted_run_proof_tests {
    use super::AgentHarness;
    use crate::harness::MemoryStore;
    use crate::types::AgentMessage;

    #[test]
    fn accepted_run_rejects_malformed_assistant_cross_run_and_forged_prefix() {
        let mut harness = AgentHarness::new(MemoryStore::new("session"));
        let accepted = harness
            .accept_prompt_and_drive("run-a", AgentMessage::user("hello", Vec::new()))
            .unwrap();
        harness.validate_accepted_run(&accepted).unwrap();

        let mut malformed = accepted.clone();
        malformed.assistant_entry_id = "entry-run-a-assistant-99".into();
        assert!(harness.validate_accepted_run(&malformed).is_err());

        let mut cross_run = accepted.clone();
        cross_run.assistant_entry_id = "entry-run-b-assistant-1".into();
        assert!(harness.validate_accepted_run(&cross_run).is_err());

        let mut oversized_prefix = accepted.clone();
        oversized_prefix.accepted_through_seq += 1;
        assert!(harness.validate_accepted_run(&oversized_prefix).is_err());

        let mut forged_prefix = accepted.clone();
        forged_prefix.accepted_through_seq -= 1;
        assert!(harness.validate_accepted_run(&forged_prefix).is_err());
        assert!(harness
            .validate_accepted_run_token("run-a", "main", forged_prefix.accepted_through_seq)
            .is_err());
    }
}
