use super::effects::{EffectAction, EffectsError, GatedEffects};
use super::store::SessionStore;
use super::types::{
    CompactionReason, Entry, OperationIntent, OperationOutcome, ProvisionedEntry, QueueKind,
    Record, ToolResult, ToolSpec, UsageCause,
};
use crate::types::{AgentMessage, DeferredHandle, TokenUsage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcedureError {
    Invalid(String),
    Effects(EffectsError),
}

impl std::fmt::Display for ProcedureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ProcedureError {}

impl From<EffectsError> for ProcedureError {
    fn from(error: EffectsError) -> Self {
        Self::Effects(error)
    }
}

/// The smallest real harness procedure: a durable prompt and one provider
/// response with no tools. Its actions are created up front, then committed by
/// the same effects gate used for manual and automatic driving.
pub struct NoToolRun;

pub struct PromptProcedure;

pub struct AssistantAttemptProcedure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: u64,
    pub max_delay: u64,
}

impl RetryPolicy {
    fn delay_for(self, attempt: u32) -> u64 {
        let shift = attempt.saturating_sub(1).min(63);
        self.base_delay
            .saturating_mul(1u64 << shift)
            .min(self.max_delay)
    }
}

pub struct RetryProcedure;

impl RetryProcedure {
    pub(crate) fn schedule<S: SessionStore>(
        store: &S,
        run_id: &str,
        reason: &str,
        policy: RetryPolicy,
        effects: &mut GatedEffects,
    ) -> Result<u32, ProcedureError> {
        if reason.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "retry reason must be non-empty".into(),
            ));
        }
        let lane = open_lane(store, run_id)?;
        if lane.retry.is_some() {
            return Err(ProcedureError::Invalid("retry is already scheduled".into()));
        }
        let attempt = next_attempt(store, run_id);
        if attempt == 0 || attempt > policy.max_attempts {
            return Err(ProcedureError::Invalid(
                "retry attempt cap exhausted".into(),
            ));
        }
        let seq = next_seq_with_effects(store, effects);
        let retry_at = seq.saturating_add(policy.delay_for(attempt));
        effects.park(EffectAction::AppendRecord {
            id: format!("retry-action-{run_id}-{attempt}"),
            record: Record::RetryScheduled {
                id: format!("retry-{run_id}-{attempt}"),
                seq,
                lane: lane.name,
                timestamp: seq,
                run_id: run_id.into(),
                attempt,
                retry_at,
                reason: reason.into(),
            },
        })?;
        Ok(attempt)
    }

    pub(crate) fn begin<S: SessionStore>(
        store: &S,
        run_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<u32, ProcedureError> {
        let lane = open_lane(store, run_id)?;
        let retry = lane
            .retry
            .ok_or_else(|| ProcedureError::Invalid("no retry is scheduled".into()))?;
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("retry-consume-action-{run_id}-{}", retry.attempt),
            record: Record::RetryConsumed {
                id: format!("retry-consumed-{run_id}-{}", retry.attempt),
                seq,
                lane: lane.name,
                timestamp: seq,
                run_id: run_id.into(),
                attempt: retry.attempt,
            },
        })?;
        Ok(retry.attempt)
    }
}

impl AssistantAttemptProcedure {
    pub(crate) fn record_usage<S: SessionStore>(
        store: &S,
        run_id: &str,
        usage: TokenUsage,
        effects: &mut GatedEffects,
    ) -> Result<u32, ProcedureError> {
        Self::record_usage_with_cause(store, run_id, usage, UsageCause::Provider, effects)
    }

    pub(crate) fn record_discarded_usage<S: SessionStore>(
        store: &S,
        run_id: &str,
        usage: TokenUsage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::record_usage_with_cause(store, run_id, usage, UsageCause::Discarded, effects)
            .map(|_| ())
    }

    pub(crate) fn record_adjustment<S: SessionStore>(
        store: &S,
        run_id: &str,
        usage: TokenUsage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::record_usage_with_cause(store, run_id, usage, UsageCause::Adjustment, effects)
            .map(|_| ())
    }

    fn record_usage_with_cause<S: SessionStore>(
        store: &S,
        run_id: &str,
        usage: TokenUsage,
        cause: UsageCause,
        effects: &mut GatedEffects,
    ) -> Result<u32, ProcedureError> {
        let lane = open_lane(store, run_id)?;
        if !matches!(cause, UsageCause::Provider) {
            let seq = next_seq_with_effects(store, effects);
            let attempt =
                matches!(cause, UsageCause::Discarded).then(|| current_attempt(store, run_id));
            effects.park(EffectAction::AppendRecord {
                id: format!("usage-action-{run_id}-{seq}"),
                record: Record::Usage {
                    id: format!("usage-{run_id}-{seq}"),
                    seq,
                    lane: lane.name,
                    timestamp: seq,
                    run_id: Some(run_id.into()),
                    cause,
                    entry_id: None,
                    tool_call_id: None,
                    attempt,
                    usage,
                },
            })?;
            return Ok(current_attempt(store, run_id));
        }
        let attempt = current_attempt(store, run_id);
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("provider-usage-action-{run_id}-{seq}"),
            record: Record::Usage {
                id: format!("provider-usage-{run_id}-{seq}"),
                seq,
                lane: lane.name,
                timestamp: seq,
                run_id: Some(run_id.into()),
                cause: UsageCause::Provider,
                entry_id: None,
                tool_call_id: None,
                attempt: Some(attempt),
                usage,
            },
        })?;
        Ok(attempt)
    }

    pub(crate) fn finish<S: SessionStore>(
        store: &S,
        run_id: &str,
        result_entry_id: &str,
        usage: TokenUsage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let lane = open_lane(store, run_id)?;
        let entry = store
            .entries()
            .iter()
            .find(|entry| entry.id == result_entry_id)
            .ok_or_else(|| ProcedureError::Invalid("assistant result does not exist".into()))?;
        if !matches!(entry.message, AgentMessage::Assistant { .. }) {
            return Err(ProcedureError::Invalid(
                "assistant attempt result is not an assistant entry".into(),
            ));
        }
        let attempt = store
            .records()
            .iter()
            .find_map(|record| match record {
                Record::StepAttempt {
                    run_id: record_run_id,
                    attempt,
                    result_entry_id: record_entry_id,
                    ..
                } if record_run_id == run_id && record_entry_id == result_entry_id => {
                    Some(*attempt)
                }
                _ => None,
            })
            .unwrap_or_else(|| current_attempt(store, run_id));
        let seq = next_seq_with_effects(store, effects);
        let step_id = format!("attempt-{run_id}-{attempt}");
        // Guard on the record id, not (run_id, result_entry_id): two
        // different result entries that happen to share the same attempt
        // number would otherwise produce colliding ids.
        if !store
            .records()
            .iter()
            .any(|record| matches!(record, Record::StepAttempt { id, .. } if id == &step_id))
        {
            effects.park(EffectAction::AppendRecord {
                id: format!("assistant-attempt-action-{run_id}-{attempt}"),
                record: Record::StepAttempt {
                    id: step_id,
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    attempt,
                    result_entry_id: result_entry_id.into(),
                    compaction_reason: None,
                },
            })?;
        }
        if !store.records().iter().any(|record| {
            matches!(record, Record::Usage { run_id: Some(record_run_id), cause: UsageCause::Provider, attempt: Some(record_attempt), .. } if record_run_id == run_id && *record_attempt == attempt)
        }) {
            effects.park(EffectAction::AppendRecord {
                id: format!("assistant-usage-action-{run_id}-{attempt}"),
                record: Record::Usage {
                    id: format!("usage-{run_id}-{attempt}"),
                    seq: seq + 1,
                    lane: lane.name.clone(),
                    timestamp: seq + 1,
                    run_id: Some(run_id.into()),
                    cause: UsageCause::Provider,
                    entry_id: Some(result_entry_id.into()),
                    tool_call_id: None,
                    attempt: Some(attempt),
                    usage,
                },
            })?;
        }
        Ok(())
    }
}

pub struct OperationProcedure;

impl OperationProcedure {
    pub(crate) fn start<S: SessionStore>(
        store: &S,
        run_id: &str,
        source_leaf_id: Option<String>,
        intent: OperationIntent,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::start_on_lane(store, "main", run_id, source_leaf_id, intent, effects)
    }

    pub(crate) fn start_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        source_leaf_id: Option<String>,
        intent: OperationIntent,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if run_id.trim().is_empty() {
            return Err(ProcedureError::Invalid("run id must be non-empty".into()));
        }
        if effects.has_pending_on_lane(lane_name) {
            return Err(ProcedureError::Invalid(format!(
                "lane {lane_name} has an uncommitted mutation"
            )));
        }
        if let Some(source_leaf_id) = &source_leaf_id {
            if !store
                .entries()
                .iter()
                .any(|entry| &entry.id == source_leaf_id)
            {
                return Err(ProcedureError::Invalid("source leaf does not exist".into()));
            }
        }
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        if reduced
            .lane(lane_name)
            .is_some_and(|lane| lane.open_operation.is_some())
        {
            return Err(ProcedureError::Invalid(format!("lane {lane_name} is busy")));
        }
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("operation-start-action-{run_id}"),
            record: Record::OperationStarted {
                id: run_id.into(),
                seq,
                lane: lane_name.into(),
                timestamp: seq,
                source_leaf_id,
                intent,
            },
        })?;
        Ok(())
    }

    pub(crate) fn finish<S: SessionStore>(
        store: &S,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let lane = open_lane(store, run_id)?;
        if outcome != OperationOutcome::Aborted
            && lane
                .tools
                .iter()
                .any(|tool| tool.run_id == run_id && !tool.completed)
        {
            return Err(ProcedureError::Invalid(
                "operation has an incomplete tool batch".into(),
            ));
        }
        if store.records().iter().any(|record| {
            matches!(record, Record::OperationFinished { run_id: record_run_id, .. } if record_run_id == run_id)
        }) {
            return Ok(());
        }
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("operation-finish-action-{run_id}"),
            record: Record::OperationFinished {
                id: format!("finish-{run_id}"),
                seq,
                lane: lane.name,
                timestamp: seq,
                run_id: run_id.into(),
                outcome,
                error,
            },
        })?;
        Ok(())
    }
}

impl PromptProcedure {
    pub(crate) fn accept<S: SessionStore>(
        store: &S,
        run_id: &str,
        prompt: AgentMessage,
        effects: &mut GatedEffects,
    ) -> Result<String, ProcedureError> {
        Self::accept_on_lane(store, "main", run_id, prompt, effects)
    }

    pub(crate) fn accept_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        prompt: AgentMessage,
        effects: &mut GatedEffects,
    ) -> Result<String, ProcedureError> {
        if lane_name.trim().is_empty() || run_id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane and run id must be non-empty".into(),
            ));
        }
        if !prompt.is_user() {
            return Err(ProcedureError::Invalid(
                "accepted prompt must be a user message".into(),
            ));
        }
        if effects.has_pending_on_lane(lane_name) {
            return Err(ProcedureError::Invalid(format!(
                "lane {lane_name} has an uncommitted mutation"
            )));
        }
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        if reduced
            .lane(lane_name)
            .is_some_and(|lane| lane.open_operation.is_some())
        {
            return Err(ProcedureError::Invalid(format!("lane {lane_name} is busy")));
        }
        let source_leaf_id = reduced
            .lane(lane_name)
            .and_then(|lane| lane.leaf_id.clone())
            .or_else(|| {
                (lane_name == "main")
                    .then(|| store.entries().last().map(|entry| entry.id.clone()))
                    .flatten()
            });
        let first_seq = next_seq_with_effects(store, effects);
        let prompt_id = format!("entry-{run_id}-user");
        let result_entry_id = format!("entry-{run_id}-assistant-1");
        effects.park(EffectAction::AppendRecord {
            id: format!("operation-start-action-{run_id}"),
            record: Record::OperationStarted {
                id: run_id.into(),
                seq: first_seq,
                lane: lane_name.into(),
                timestamp: first_seq,
                source_leaf_id: source_leaf_id.clone(),
                intent: OperationIntent::Run,
            },
        })?;
        effects.park(EffectAction::AppendEntry {
            entry: Entry {
                id: prompt_id.clone(),
                parent_id: source_leaf_id,
                lane: lane_name.into(),
                seq: first_seq + 1,
                timestamp: first_seq + 1,
                message: prompt,
                surface_op: super::types::SurfaceOperation::Append,
                terminate: false,
            },
        })?;
        effects.park(EffectAction::AppendRecord {
            id: format!("assistant-attempt-action-{run_id}-1"),
            record: Record::StepAttempt {
                id: format!("attempt-{run_id}-1"),
                seq: first_seq + 2,
                lane: lane_name.into(),
                timestamp: first_seq + 2,
                run_id: run_id.into(),
                attempt: 1,
                result_entry_id: result_entry_id.clone(),
                compaction_reason: None,
            },
        })?;
        Ok(result_entry_id)
    }
}

impl NoToolRun {
    pub fn accept<S: SessionStore>(
        store: &S,
        run_id: &str,
        prompt: &str,
        assistant: AgentMessage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::accept_on_lane(store, "main", run_id, prompt, assistant, effects)
    }

    pub(crate) fn accept_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        prompt: &str,
        assistant: AgentMessage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if run_id.trim().is_empty() || prompt.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "run id and prompt must be non-empty".into(),
            ));
        }
        if effects.has_pending_on_lane(lane_name) {
            return Err(ProcedureError::Invalid(format!(
                "lane {lane_name} has an uncommitted mutation"
            )));
        }
        let lane = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?
            .lane(lane_name)
            .cloned()
            .ok_or_else(|| ProcedureError::Invalid(format!("lane {lane_name} is missing")))?;
        if lane.open_operation.is_some() {
            return Err(ProcedureError::Invalid(format!("lane {lane_name} is busy")));
        }

        let source_leaf_id = lane.leaf_id.clone().or_else(|| {
            (lane_name == "main")
                .then(|| store.entries().last().map(|entry| entry.id.clone()))
                .flatten()
        });
        let first_seq = next_seq_with_effects(store, effects);
        let user_id = format!("entry-{run_id}-user");
        let assistant_id = format!("entry-{run_id}-assistant");
        let actions = [
            EffectAction::AppendRecord {
                id: format!("record-{run_id}-started"),
                record: Record::OperationStarted {
                    id: run_id.into(),
                    seq: first_seq,
                    lane: lane_name.into(),
                    timestamp: first_seq,
                    source_leaf_id: source_leaf_id.clone(),
                    intent: OperationIntent::Run,
                },
            },
            EffectAction::AppendEntry {
                entry: Entry {
                    id: user_id.clone(),
                    parent_id: source_leaf_id,
                    lane: lane_name.into(),
                    seq: first_seq + 1,
                    timestamp: first_seq + 1,
                    message: AgentMessage::user(prompt, Vec::new()),
                    surface_op: super::types::SurfaceOperation::Append,
                    terminate: false,
                },
            },
            EffectAction::AppendRecord {
                id: format!("record-{run_id}-attempt"),
                record: Record::StepAttempt {
                    id: format!("attempt-{run_id}"),
                    seq: first_seq + 2,
                    lane: lane_name.into(),
                    timestamp: first_seq + 2,
                    run_id: run_id.into(),
                    attempt: 1,
                    result_entry_id: assistant_id.clone(),
                    compaction_reason: None,
                },
            },
            EffectAction::AppendEntry {
                entry: Entry {
                    id: assistant_id,
                    parent_id: Some(user_id),
                    lane: lane_name.into(),
                    seq: first_seq + 3,
                    timestamp: first_seq + 3,
                    message: assistant,
                    surface_op: super::types::SurfaceOperation::Append,
                    terminate: false,
                },
            },
            EffectAction::AppendRecord {
                id: format!("record-{run_id}-finished"),
                record: Record::OperationFinished {
                    id: format!("finish-{run_id}"),
                    seq: first_seq + 4,
                    lane: lane_name.into(),
                    timestamp: first_seq + 4,
                    run_id: run_id.into(),
                    outcome: OperationOutcome::Completed,
                    error: None,
                },
            },
        ];
        for action in actions {
            effects.park(action)?;
        }
        Ok(())
    }

    pub fn resume<S: SessionStore>(
        store: &S,
        run_id: &str,
        prompt: &str,
        assistant: AgentMessage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::resume_on_lane(store, "main", run_id, prompt, assistant, effects)
    }

    pub(crate) fn resume_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        prompt: &str,
        assistant: AgentMessage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        if reduced
            .lane(lane_name)
            .and_then(|lane| lane.open_operation.as_deref())
            != Some(run_id)
        {
            return Err(ProcedureError::Invalid("operation is not open".into()));
        }
        if effects.has_pending_on_lane(lane_name) {
            return Err(ProcedureError::Invalid(format!(
                "lane {lane_name} has an uncommitted mutation"
            )));
        }
        let user_id = format!("entry-{run_id}-user");
        let assistant_id = format!("entry-{run_id}-assistant");
        let source_leaf_id = store.records().iter().find_map(|record| match record {
            Record::OperationStarted {
                id, source_leaf_id, ..
            } if id == run_id => source_leaf_id.clone(),
            _ => None,
        });
        let mut seq = next_seq_with_effects(store, effects);
        if !store.entries().iter().any(|entry| entry.id == user_id) {
            effects.park(EffectAction::AppendEntry {
                entry: Entry {
                    id: user_id.clone(),
                    parent_id: source_leaf_id,
                    lane: lane_name.into(),
                    seq,
                    timestamp: seq,
                    message: AgentMessage::user(prompt, Vec::new()),
                    surface_op: super::types::SurfaceOperation::Append,
                    terminate: false,
                },
            })?;
            seq += 1;
        }
        if !store.records().iter().any(|record| {
            matches!(record, Record::StepAttempt { id, .. } if id == &format!("attempt-{run_id}"))
        }) {
            effects.park(EffectAction::AppendRecord {
                id: format!("record-{run_id}-attempt"),
                record: Record::StepAttempt {
                    id: format!("attempt-{run_id}"),
                    seq,
                    lane: lane_name.into(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    attempt: 1,
                    result_entry_id: assistant_id.clone(),
                    compaction_reason: None,
                },
            })?;
            seq += 1;
        }
        if !store.entries().iter().any(|entry| entry.id == assistant_id) {
            effects.park(EffectAction::AppendEntry {
                entry: Entry {
                    id: assistant_id,
                    parent_id: Some(user_id),
                    lane: lane_name.into(),
                    seq,
                    timestamp: seq,
                    message: assistant,
                    surface_op: super::types::SurfaceOperation::Append,
                    terminate: false,
                },
            })?;
            seq += 1;
        }
        if !store.records().iter().any(
            |record| matches!(record, Record::OperationFinished { run_id: id, .. } if id == run_id),
        ) {
            effects.park(EffectAction::AppendRecord {
                id: format!("record-{run_id}-finished"),
                record: Record::OperationFinished {
                    id: format!("finish-{run_id}"),
                    seq,
                    lane: lane_name.into(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    outcome: OperationOutcome::Completed,
                    error: None,
                },
            })?;
        }
        Ok(())
    }

    fn resume_navigation<S: SessionStore>(
        store: &S,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::resume_navigation_on_lane(store, "main", run_id, target_leaf_id, summary, effects)
    }

    fn resume_navigation_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        if reduced
            .lane(lane_name)
            .and_then(|lane| lane.open_operation.as_deref())
            != Some(run_id)
        {
            return Err(ProcedureError::Invalid("operation is not open".into()));
        }
        if !store
            .entries()
            .iter()
            .any(|entry| entry.id == target_leaf_id)
        {
            return Err(ProcedureError::Invalid("target leaf does not exist".into()));
        }
        let moved = store.records().iter().any(|record| {
            matches!(record, Record::LaneMoved { run_id: record_run_id, target_leaf_id: target, .. } if record_run_id == run_id && target == target_leaf_id)
        });
        let mut seq = next_seq_with_effects(store, effects);
        if !moved {
            effects.park(EffectAction::AppendRecord {
                id: format!("navigation-move-action-{run_id}"),
                record: Record::LaneMoved {
                    id: format!("navigation-moved-{run_id}"),
                    seq,
                    lane: lane_name.into(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    target_leaf_id: target_leaf_id.into(),
                },
            })?;
            seq += 1;
        }
        let summary_id = format!("navigation-{run_id}-summary");
        if let Some(summary) = summary {
            let nav_attempt_id = format!("navigation-attempt-{run_id}");
            // Guard on the record id, not (run_id, result_entry_id): a
            // second navigation summary for the same run would otherwise
            // collide.
            let attempt_exists = store.records().iter().any(
                |record| matches!(record, Record::StepAttempt { id, .. } if id == &nav_attempt_id),
            );
            if !attempt_exists {
                effects.park(EffectAction::AppendRecord {
                    id: format!("navigation-attempt-action-{run_id}"),
                    record: Record::StepAttempt {
                        id: nav_attempt_id,
                        seq,
                        lane: lane_name.into(),
                        timestamp: seq,
                        run_id: run_id.into(),
                        attempt: 1,
                        result_entry_id: summary_id.clone(),
                        compaction_reason: Some("navigation".into()),
                    },
                })?;
                seq += 1;
            }
            if !store.entries().iter().any(|entry| entry.id == summary_id) {
                effects.park(EffectAction::AppendEntry {
                    entry: Entry {
                        id: summary_id,
                        parent_id: Some(target_leaf_id.into()),
                        lane: lane_name.into(),
                        seq,
                        timestamp: seq,
                        message: AgentMessage::Custom {
                            custom_type: "navigation_summary".into(),
                            payload: serde_json::json!({"text": summary}),
                        },
                        surface_op: super::types::SurfaceOperation::Append,
                        terminate: false,
                    },
                })?;
                seq += 1;
            }
        }
        if !store.records().iter().any(|record| {
            matches!(record, Record::OperationFinished { run_id: record_run_id, .. } if record_run_id == run_id)
        }) {
            effects.park(EffectAction::AppendRecord {
                id: format!("navigation-finish-action-{run_id}"),
                record: Record::OperationFinished {
                    id: format!("navigation-finished-{run_id}"),
                    seq,
                    lane: lane_name.into(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    outcome: OperationOutcome::Completed,
                    error: None,
                },
            })?;
        }
        Ok(())
    }
}

pub struct NavigationProcedure;

impl NavigationProcedure {
    pub fn resume<S: SessionStore>(
        store: &S,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        NoToolRun::resume_navigation(store, run_id, target_leaf_id, summary, effects)
    }

    pub fn resume_on_lane<S: SessionStore>(
        store: &S,
        lane: &str,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        NoToolRun::resume_navigation_on_lane(store, lane, run_id, target_leaf_id, summary, effects)
    }

    pub fn accept<S: SessionStore>(
        store: &S,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::accept_on_lane(store, "main", run_id, target_leaf_id, summary, effects)
    }

    pub(crate) fn accept_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        target_leaf_id: &str,
        summary: Option<String>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if run_id.trim().is_empty() || target_leaf_id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "run id and target leaf must be non-empty".into(),
            ));
        }
        let lane = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?
            .lane(lane_name)
            .cloned()
            .ok_or_else(|| ProcedureError::Invalid(format!("lane {lane_name} is missing")))?;
        if lane.open_operation.is_some() {
            return Err(ProcedureError::Invalid(format!("lane {lane_name} is busy")));
        }
        if !store
            .entries()
            .iter()
            .any(|entry| entry.id == target_leaf_id)
        {
            return Err(ProcedureError::Invalid("target leaf does not exist".into()));
        }
        let first_seq = next_seq_with_effects(store, effects);
        let source_leaf_id = lane.leaf_id.clone().or_else(|| {
            (lane_name == "main")
                .then(|| store.entries().last().map(|entry| entry.id.clone()))
                .flatten()
        });
        let summary_id = format!("navigation-{run_id}-summary");
        let mut actions = vec![EffectAction::AppendRecord {
            id: format!("navigation-start-action-{run_id}"),
            record: Record::OperationStarted {
                id: run_id.into(),
                seq: first_seq,
                lane: lane_name.into(),
                timestamp: first_seq,
                source_leaf_id,
                intent: OperationIntent::Navigation,
            },
        }];
        let mut seq = first_seq + 1;
        if let Some(summary) = summary {
            actions.push(EffectAction::AppendRecord {
                id: format!("navigation-move-action-{run_id}"),
                record: Record::LaneMoved {
                    id: format!("navigation-moved-{run_id}"),
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    target_leaf_id: target_leaf_id.into(),
                },
            });
            seq += 1;
            actions.push(EffectAction::AppendRecord {
                id: format!("navigation-attempt-action-{run_id}"),
                record: Record::StepAttempt {
                    id: format!("navigation-attempt-{run_id}"),
                    seq,
                    lane: lane_name.into(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    attempt: 1,
                    result_entry_id: summary_id.clone(),
                    compaction_reason: Some("navigation".into()),
                },
            });
            seq += 1;
            actions.push(EffectAction::AppendEntry {
                entry: Entry {
                    id: summary_id.clone(),
                    parent_id: Some(target_leaf_id.into()),
                    lane: lane_name.into(),
                    seq,
                    timestamp: seq,
                    message: AgentMessage::Custom {
                        custom_type: "navigation_summary".into(),
                        payload: serde_json::json!({"text": summary}),
                    },
                    surface_op: super::types::SurfaceOperation::Append,
                    terminate: false,
                },
            });
            seq += 1;
        } else {
            actions.push(EffectAction::AppendRecord {
                id: format!("navigation-move-action-{run_id}"),
                record: Record::LaneMoved {
                    id: format!("navigation-moved-{run_id}"),
                    seq,
                    lane: lane_name.into(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    target_leaf_id: target_leaf_id.into(),
                },
            });
            seq += 1;
        }
        actions.push(EffectAction::AppendRecord {
            id: format!("navigation-finish-action-{run_id}"),
            record: Record::OperationFinished {
                id: format!("navigation-finished-{run_id}"),
                seq,
                lane: lane.name.clone(),
                timestamp: seq,
                run_id: run_id.into(),
                outcome: OperationOutcome::Completed,
                error: None,
            },
        });
        for action in actions {
            effects.park(action)?;
        }
        Ok(())
    }
}

pub struct CompactionProcedure;

impl CompactionProcedure {
    pub(crate) fn accept<S: SessionStore>(
        store: &S,
        run_id: &str,
        summary: &str,
        context_snapshot_index: &[serde_json::Value],
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::accept_on_lane(
            store,
            "main",
            run_id,
            summary,
            context_snapshot_index,
            effects,
        )
    }

    pub(crate) fn checkpoint_open_run<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        summary: &str,
        context_snapshot_index: &[serde_json::Value],
        reason: CompactionReason,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() || run_id.trim().is_empty() || summary.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name, run id, and summary must be non-empty".into(),
            ));
        }
        let lane = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?
            .lane(lane_name)
            .cloned()
            .ok_or_else(|| ProcedureError::Invalid(format!("lane {lane_name} is missing")))?;
        if lane.open_operation.as_deref() != Some(run_id) {
            return Err(ProcedureError::Invalid(format!(
                "operation {run_id} is not open on lane {lane_name}"
            )));
        }
        let first_seq = next_seq_with_effects(store, effects);
        let source_leaf_id = lane.leaf_id;
        let summary_id = format!("compaction-{run_id}-{first_seq}-summary");
        effects.park(EffectAction::AppendEntry {
            entry: Entry {
                id: summary_id.clone(),
                parent_id: None,
                lane: lane_name.into(),
                seq: first_seq,
                timestamp: first_seq,
                message: AgentMessage::Custom {
                    custom_type: "compaction_summary".into(),
                    payload: serde_json::json!({
                        "schema_version": 1,
                        "summary": summary,
                        "checkpoint_kind": reason.as_str(),
                        "source_leaf_id": source_leaf_id,
                        "context_snapshot_index": context_snapshot_index,
                    }),
                },
                surface_op: super::types::SurfaceOperation::Replace {
                    start_seq: 1,
                    end_seq: first_seq.saturating_sub(1),
                    source_event_seqs: Vec::new(),
                },
                terminate: false,
            },
        })?;
        effects.park(EffectAction::AppendRecord {
            id: format!("compaction-move-action-{run_id}-{first_seq}"),
            record: Record::LaneMoved {
                id: format!("compaction-move-{run_id}-{first_seq}"),
                seq: first_seq + 1,
                lane: lane_name.into(),
                timestamp: first_seq + 1,
                run_id: run_id.into(),
                target_leaf_id: summary_id,
            },
        })?;
        Ok(())
    }
    pub(crate) fn accept_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        summary: &str,
        context_snapshot_index: &[serde_json::Value],
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if run_id.trim().is_empty() || summary.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "run id and summary must be non-empty".into(),
            ));
        }
        let lane = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?
            .lane(lane_name)
            .cloned()
            .ok_or_else(|| ProcedureError::Invalid(format!("lane {lane_name} is missing")))?;
        if lane.open_operation.is_some() {
            return Err(ProcedureError::Invalid(format!("lane {lane_name} is busy")));
        }
        let first_seq = next_seq_with_effects(store, effects);
        let source_leaf_id = lane.leaf_id.clone().or_else(|| {
            (lane_name == "main")
                .then(|| store.entries().last().map(|entry| entry.id.clone()))
                .flatten()
        });
        let summary_id = format!("compaction-{run_id}-summary");
        let summary_entry_id = summary_id.clone();
        let actions = [
            EffectAction::AppendRecord {
                id: format!("compaction-start-action-{run_id}"),
                record: Record::OperationStarted {
                    id: run_id.into(),
                    seq: first_seq,
                    lane: lane_name.into(),
                    timestamp: first_seq,
                    source_leaf_id: source_leaf_id.clone(),
                    intent: OperationIntent::Compaction,
                },
            },
            EffectAction::AppendEntry {
                entry: Entry {
                    id: summary_id.clone(),
                    // Compaction is an append-only branch reset: the old
                    // entries remain navigable, but the compacted context
                    // must not traverse them.
                    parent_id: None,
                    lane: lane_name.into(),
                    seq: first_seq + 1,
                    timestamp: first_seq + 1,
                    message: AgentMessage::Custom {
                        custom_type: "compaction_summary".into(),
                        payload: serde_json::json!({
                            "schema_version": 1,
                            "summary": summary,
                            "checkpoint_kind": "manual",
                            "source_leaf_id": source_leaf_id,
                            "context_snapshot_index": context_snapshot_index,
                        }),
                    },
                    surface_op: super::types::SurfaceOperation::Replace {
                        start_seq: 1,
                        end_seq: first_seq,
                        source_event_seqs: Vec::new(),
                    },
                    terminate: false,
                },
            },
            EffectAction::AppendRecord {
                id: format!("compaction-attempt-action-{run_id}"),
                record: Record::StepAttempt {
                    id: format!("compaction-attempt-{run_id}"),
                    seq: first_seq + 2,
                    lane: lane_name.into(),
                    timestamp: first_seq + 2,
                    run_id: run_id.into(),
                    attempt: 1,
                    result_entry_id: summary_id.clone(),
                    compaction_reason: Some("manual".into()),
                },
            },
            EffectAction::AppendRecord {
                id: format!("compaction-usage-action-{run_id}"),
                record: Record::Usage {
                    id: format!("compaction-usage-{run_id}"),
                    seq: first_seq + 3,
                    lane: lane_name.into(),
                    timestamp: first_seq + 3,
                    run_id: Some(run_id.into()),
                    cause: UsageCause::Compaction,
                    entry_id: Some(summary_entry_id),
                    tool_call_id: None,
                    attempt: Some(1),
                    usage: TokenUsage::default(),
                },
            },
            EffectAction::AppendRecord {
                id: format!("compaction-finish-action-{run_id}"),
                record: Record::OperationFinished {
                    id: format!("compaction-finished-{run_id}"),
                    seq: first_seq + 4,
                    lane: lane_name.into(),
                    timestamp: first_seq + 4,
                    run_id: run_id.into(),
                    outcome: OperationOutcome::Completed,
                    error: None,
                },
            },
        ];
        for action in actions {
            effects.park(action)?;
        }
        Ok(())
    }
}

pub struct QueueProcedure;

impl QueueProcedure {
    pub(crate) fn enqueue_unbound<S: SessionStore>(
        store: &S,
        queue: QueueKind,
        target: ProvisionedEntry,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::enqueue_unbound_on_lane(store, "main", queue, target, effects)
    }

    pub(crate) fn enqueue_unbound_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        queue: QueueKind,
        target: ProvisionedEntry,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if target.id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "queued entry id must be non-empty".into(),
            ));
        }
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        if reduced
            .lane(lane_name)
            .is_some_and(|lane| lane.queued.iter().any(|entry| entry.target.id == target.id))
        {
            return Err(ProcedureError::Invalid(
                "queued entry already exists".into(),
            ));
        }
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("queue-action-{}", target.id),
            record: Record::QueueEnqueued {
                id: format!("queue-{}", target.id),
                seq,
                lane: lane_name.into(),
                timestamp: seq,
                run_id: None,
                queue,
                priority: None,
                target,
            },
        })?;
        Ok(())
    }

    pub fn enqueue<S: SessionStore>(
        store: &S,
        run_id: &str,
        queue: QueueKind,
        target: ProvisionedEntry,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::enqueue_on_lane(store, "main", run_id, queue, target, effects)
    }

    pub(crate) fn enqueue_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        queue: QueueKind,
        target: ProvisionedEntry,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if run_id.trim().is_empty() || target.id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "run id and queued entry id must be non-empty".into(),
            ));
        }
        let lane = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        if lane
            .lane(lane_name)
            .is_some_and(|lane| lane.queued.iter().any(|entry| entry.target.id == target.id))
        {
            return Err(ProcedureError::Invalid(
                "queued entry already exists".into(),
            ));
        }
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("queue-action-{}", target.id),
            record: Record::QueueEnqueued {
                id: format!("queue-{}", target.id),
                seq,
                lane: lane_name.into(),
                timestamp: seq,
                run_id: Some(run_id.into()),
                queue,
                priority: None,
                target,
            },
        })?;
        Ok(())
    }

    pub fn cancel<S: SessionStore>(
        store: &S,
        run_id: &str,
        entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::cancel_on_lane(store, "main", run_id, entry_id, effects)
    }

    pub(crate) fn cancel_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if run_id.trim().is_empty() || entry_id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "run id and queued entry id must be non-empty".into(),
            ));
        }
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        if !reduced.lane(lane_name).is_some_and(|lane| {
            lane.queued
                .iter()
                .any(|entry| entry.target.id == entry_id && entry.run_id.as_deref() == Some(run_id))
        }) {
            return Err(ProcedureError::Invalid(
                "queued entry does not exist".into(),
            ));
        }
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("cancel-action-{entry_id}-{seq}"),
            record: Record::QueueCancelled {
                id: format!("cancel-{entry_id}-{seq}"),
                seq,
                lane: lane_name.into(),
                timestamp: seq,
                run_id: run_id.into(),
                entry_id: entry_id.into(),
            },
        })?;
        Ok(())
    }

    pub(crate) fn cancel_unbound<S: SessionStore>(
        store: &S,
        entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::cancel_unbound_on_lane(store, "main", entry_id, effects)
    }

    pub(crate) fn cancel_unbound_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if entry_id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "queued entry id must be non-empty".into(),
            ));
        }
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        let queued = reduced
            .lane(lane_name)
            .and_then(|lane| lane.queued.iter().find(|entry| entry.target.id == entry_id))
            .ok_or_else(|| ProcedureError::Invalid("queued entry does not exist".into()))?;
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("cancel-action-{entry_id}-{seq}"),
            record: Record::QueueCancelled {
                id: format!("cancel-{entry_id}-{seq}"),
                seq,
                lane: lane_name.into(),
                timestamp: seq,
                run_id: queued.run_id.clone().unwrap_or_default(),
                entry_id: entry_id.into(),
            },
        })?;
        Ok(())
    }

    pub fn consume<S: SessionStore>(
        store: &S,
        run_id: &str,
        entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::consume_on_lane(store, "main", run_id, entry_id, effects)
    }

    pub(crate) fn consume_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        run_id: &str,
        entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if run_id.trim().is_empty() || entry_id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "run id and queued entry id must be non-empty".into(),
            ));
        }
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        if !reduced.lane(lane_name).is_some_and(|lane| {
            lane.queued
                .iter()
                .any(|entry| entry.target.id == entry_id && entry.run_id.as_deref() == Some(run_id))
        }) {
            return Err(ProcedureError::Invalid(
                "queued entry does not exist".into(),
            ));
        }
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("consume-action-{entry_id}-{seq}"),
            record: Record::QueueConsumed {
                id: format!("consume-{entry_id}-{seq}"),
                seq,
                lane: lane_name.into(),
                timestamp: seq,
                run_id: run_id.into(),
                entry_id: entry_id.into(),
            },
        })?;
        Ok(())
    }

    pub(crate) fn consume_unbound<S: SessionStore>(
        store: &S,
        entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::consume_unbound_on_lane(store, "main", entry_id, effects)
    }

    pub(crate) fn consume_unbound_on_lane<S: SessionStore>(
        store: &S,
        lane_name: &str,
        entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if lane_name.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "lane name must be non-empty".into(),
            ));
        }
        if entry_id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "queued entry id must be non-empty".into(),
            ));
        }
        let reduced = super::Reducer::reduce(store)
            .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
        let queued = reduced
            .lane(lane_name)
            .and_then(|lane| lane.queued.iter().find(|entry| entry.target.id == entry_id))
            .ok_or_else(|| ProcedureError::Invalid("queued entry does not exist".into()))?;
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("consume-action-{entry_id}-{seq}"),
            record: Record::QueueConsumed {
                id: format!("consume-{entry_id}-{seq}"),
                seq,
                lane: lane_name.into(),
                timestamp: seq,
                run_id: queued.run_id.clone().unwrap_or_default(),
                entry_id: entry_id.into(),
            },
        })?;
        Ok(())
    }
}

pub struct AbortProcedure;

impl AbortProcedure {
    pub(crate) fn request<S: SessionStore>(
        store: &S,
        run_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if run_id.trim().is_empty() {
            return Err(ProcedureError::Invalid("run id must be non-empty".into()));
        }
        let lane = open_lane(store, run_id)?;
        if effects.has_pending_on_lane(&lane.name) {
            return Err(ProcedureError::Invalid(format!(
                "lane {} has an uncommitted mutation",
                lane.name
            )));
        }
        if lane.open_operation.as_deref() != Some(run_id) {
            return Err(ProcedureError::Invalid("operation is not open".into()));
        }
        if lane.abort_requested {
            return Err(ProcedureError::Invalid("abort is already requested".into()));
        }
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("abort-action-{run_id}-{seq}"),
            record: Record::AbortRequested {
                id: format!("abort-{run_id}-{seq}"),
                seq,
                lane: lane.name.clone(),
                timestamp: seq,
                run_id: run_id.into(),
            },
        })?;
        Ok(())
    }

    pub(crate) fn reconcile<S: SessionStore>(
        store: &S,
        run_id: &str,
        assistant_entry_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let lane = open_lane(store, run_id)?;
        if effects.has_pending_on_lane(&lane.name) {
            return Err(ProcedureError::Invalid(format!(
                "lane {} has an uncommitted mutation",
                lane.name
            )));
        }
        if !lane.abort_requested {
            return Err(ProcedureError::Invalid(
                "abort has not been requested".into(),
            ));
        }
        let assistant_exists = store
            .entries()
            .iter()
            .any(|entry| entry.id == assistant_entry_id);
        if !assistant_exists && assistant_entry_id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "assistant result identity is empty".into(),
            ));
        }
        let mut seq = next_seq_with_effects(store, effects);
        let mut parent_id = lane
            .leaf_id
            .clone()
            .or_else(|| {
                (lane.name == "main")
                    .then(|| store.entries().last().map(|entry| entry.id.clone()))
                    .flatten()
            })
            .or_else(|| assistant_exists.then(|| assistant_entry_id.into()));
        DeferredProcedure::apply_pending(store, run_id, effects)?;
        seq += lane
            .deferred_writes
            .iter()
            .map(|target| {
                if store.entries().iter().any(|entry| entry.id == target.id) {
                    1
                } else {
                    2
                }
            })
            .sum::<u64>();
        for tool in lane.tools.iter().filter(|tool| !tool.completed) {
            if !store
                .entries()
                .iter()
                .any(|entry| entry.id == tool.result_entry_id)
            {
                effects.park(EffectAction::AppendEntry {
                    entry: Entry {
                        id: tool.result_entry_id.clone(),
                        parent_id: parent_id.clone(),
                        lane: lane.name.clone(),
                        seq,
                        timestamp: seq,
                        message: AgentMessage::Tool {
                            tool_call_id: tool.tool_call_id.clone(),
                            name: tool.tool_name.clone(),
                            content: "Tool execution was interrupted by abort.".into(),
                            is_error: true,
                            terminate: false,
                        },
                        surface_op: super::types::SurfaceOperation::Append,
                        terminate: false,
                    },
                })?;
                seq += 1;
                parent_id = Some(tool.result_entry_id.clone());
            }
            effects.park(EffectAction::AppendRecord {
                id: format!("abort-tool-finish-action-{run_id}-{}", tool.tool_call_id),
                record: Record::ToolFinished {
                    id: format!("abort-tool-finished-{run_id}-{}", tool.tool_call_id),
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    tool_call_id: tool.tool_call_id.clone(),
                    result_entry_id: tool.result_entry_id.clone(),
                    terminate: false,
                },
            })?;
            seq += 1;
        }
        for queued in lane
            .queued
            .iter()
            .filter(|queued| !matches!(queued.queue, QueueKind::NextRun))
        {
            effects.park(EffectAction::AppendRecord {
                id: format!("abort-queue-cancel-action-{run_id}-{}", queued.target.id),
                record: Record::QueueCancelled {
                    id: format!("abort-queue-cancel-{run_id}-{}", queued.target.id),
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: queued.run_id.clone().unwrap_or_default(),
                    entry_id: queued.target.id.clone(),
                },
            })?;
            seq += 1;
        }
        let closing_id = if assistant_exists {
            format!("entry-{run_id}-aborted")
        } else {
            assistant_entry_id.to_owned()
        };
        if !store.entries().iter().any(|entry| entry.id == closing_id) {
            effects.park(EffectAction::AppendEntry {
                entry: Entry {
                    id: closing_id,
                    parent_id,
                    lane: lane.name.clone(),
                    seq,
                    timestamp: seq,
                    message: AgentMessage::Assistant {
                        content: Some("Run aborted.".into()),
                        tool_calls: None,
                        stop_reason: Some("aborted".into()),
                        deferred_handle: None,
                    },
                    surface_op: super::types::SurfaceOperation::Append,
                    terminate: false,
                },
            })?;
            seq += 1;
        }
        if !store.records().iter().any(|record| {
            matches!(record, Record::OperationFinished { run_id: record_run_id, .. } if record_run_id == run_id)
        }) {
            effects.park(EffectAction::AppendRecord {
                id: format!("abort-finish-action-{run_id}"),
                record: Record::OperationFinished {
                    id: format!("abort-finished-{run_id}"),
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    outcome: OperationOutcome::Aborted,
                    error: None,
                },
            })?;
        }
        Ok(())
    }
}

pub struct DeferredProcedure;

#[derive(Debug, Clone, PartialEq)]
pub enum DeferredResolution {
    Pending(DeferredHandle),
    Ready(AgentMessage),
    Error(String),
}

impl DeferredProcedure {
    pub fn suspend<S: SessionStore>(
        store: &S,
        run_id: &str,
        entry: Entry,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let lane = open_lane(store, run_id)?;
        if !matches!(
            &entry.message,
            AgentMessage::Assistant {
                deferred_handle: Some(_),
                ..
            }
        ) {
            return Err(ProcedureError::Invalid(
                "deferred entry must contain a provider handle".into(),
            ));
        }
        if lane
            .deferred_writes
            .iter()
            .any(|target| target.id == entry.id)
            || store
                .entries()
                .iter()
                .any(|candidate| candidate.id == entry.id)
        {
            return Err(ProcedureError::Invalid(
                "deferred entry already exists".into(),
            ));
        }
        let mut entry = entry;
        entry.seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendEntry { entry })?;
        Ok(())
    }

    pub fn enqueue<S: SessionStore>(
        store: &S,
        run_id: &str,
        target: ProvisionedEntry,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        if run_id.trim().is_empty() || target.id.trim().is_empty() {
            return Err(ProcedureError::Invalid(
                "run id and deferred entry id must be non-empty".into(),
            ));
        }
        let lane = open_lane(store, run_id)?;
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("deferred-action-{}", target.id),
            record: Record::WriteDeferred {
                id: format!("deferred-{}", target.id),
                seq,
                lane: lane.name.clone(),
                timestamp: seq,
                run_id: run_id.into(),
                target,
            },
        })?;
        Ok(())
    }

    pub(crate) fn apply_pending<S: SessionStore>(
        store: &S,
        run_id: &str,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let lane = open_lane(store, run_id)?;
        let mut seq = next_seq_with_effects(store, effects);
        let mut parent_id = lane.leaf_id.clone().or_else(|| {
            (lane.name == "main")
                .then(|| store.entries().last().map(|entry| entry.id.clone()))
                .flatten()
        });
        for target in &lane.deferred_writes {
            if let Some(parent) = target.parent_id.as_deref() {
                if !store.entries().iter().any(|entry| entry.id == parent) {
                    return Err(ProcedureError::Invalid(
                        "deferred write parent does not exist".into(),
                    ));
                }
            }
            let already_written = store.entries().iter().any(|entry| entry.id == target.id);
            if !already_written {
                let parent = target.parent_id.clone().or_else(|| parent_id.clone());
                effects.park(EffectAction::AppendEntry {
                    entry: Entry {
                        id: target.id.clone(),
                        parent_id: parent.clone(),
                        lane: lane.name.clone(),
                        seq,
                        timestamp: seq,
                        message: target.message.clone(),
                        surface_op: super::types::SurfaceOperation::Append,
                        terminate: false,
                    },
                })?;
                seq += 1;
            }
            effects.park(EffectAction::AppendRecord {
                id: format!("deferred-applied-action-{run_id}-{}", target.id),
                record: Record::WriteApplied {
                    id: format!("deferred-applied-{run_id}-{}", target.id),
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    entry_id: target.id.clone(),
                },
            })?;
            seq += 1;
            parent_id = Some(target.id.clone());
        }
        Ok(())
    }

    pub(crate) fn redeem<S: SessionStore>(
        store: &S,
        run_id: &str,
        resolution: DeferredResolution,
        effects: &mut GatedEffects,
    ) -> Result<bool, ProcedureError> {
        let lane = open_lane(store, run_id)?;
        let start_seq = store
            .records()
            .iter()
            .find_map(|record| match record {
                Record::OperationStarted { id, seq, .. } if id == run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| ProcedureError::Invalid("operation start is missing".into()))?;
        let deferred = store
            .entries()
            .iter()
            .filter(|entry| entry.seq > start_seq)
            .find_map(|entry| match &entry.message {
                AgentMessage::Assistant {
                    deferred_handle: Some(handle),
                    ..
                } => Some((entry.id.clone(), handle.clone())),
                _ => None,
            })
            .ok_or_else(|| ProcedureError::Invalid("deferred assistant entry is missing".into()))?;
        if let DeferredResolution::Pending(handle) = resolution {
            if handle != deferred.1 {
                return Err(ProcedureError::Invalid(
                    "deferred handle changed while suspended".into(),
                ));
            }
            let seq = next_seq_with_effects(store, effects);
            effects.park(EffectAction::AppendRecord {
                id: format!("deferred-pending-usage-action-{run_id}-{seq}"),
                record: Record::Usage {
                    id: format!("deferred-pending-usage-{run_id}-{seq}"),
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: Some(run_id.into()),
                    cause: UsageCause::Provider,
                    entry_id: Some(deferred.0.clone()),
                    tool_call_id: None,
                    attempt: Some(lane.attempts.saturating_add(1)),
                    usage: TokenUsage::default(),
                },
            })?;
            return Ok(false);
        }
        let message = match resolution {
            DeferredResolution::Ready(message) => message,
            DeferredResolution::Error(error) => AgentMessage::Assistant {
                content: Some(error),
                tool_calls: None,
                stop_reason: Some("deferred_error".into()),
                deferred_handle: None,
            },
            DeferredResolution::Pending(_) => unreachable!(),
        };
        if !matches!(
            message,
            AgentMessage::Assistant {
                deferred_handle: None,
                ..
            }
        ) {
            return Err(ProcedureError::Invalid(
                "deferred redemption must be a terminal assistant message".into(),
            ));
        }
        let entry_id = format!("deferred-result-{run_id}");
        if store.entries().iter().any(|entry| entry.id == entry_id) {
            return Err(ProcedureError::Invalid(
                "deferred result already exists".into(),
            ));
        }
        let usage_entry_id = entry_id.clone();
        let attempt = lane.attempts.saturating_add(1);
        let base = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendEntry {
            entry: Entry {
                id: entry_id.clone(),
                parent_id: Some(deferred.0),
                lane: lane.name.clone(),
                seq: base,
                timestamp: base + 1,
                message,
                surface_op: super::types::SurfaceOperation::Append,
                terminate: false,
            },
        })?;
        effects.park(EffectAction::AppendRecord {
            id: format!("deferred-attempt-{run_id}"),
            record: Record::StepAttempt {
                id: format!("deferred-attempt-record-{run_id}"),
                seq: base + 3,
                lane: lane.name.clone(),
                timestamp: base + 4,
                run_id: run_id.into(),
                attempt,
                result_entry_id: entry_id,
                compaction_reason: None,
            },
        })?;
        let usage_seq = base + 6;
        effects.park(EffectAction::AppendRecord {
            id: format!("deferred-usage-action-{run_id}-{usage_seq}"),
            record: Record::Usage {
                id: format!("deferred-usage-{run_id}-{usage_seq}"),
                seq: usage_seq,
                lane: lane.name.clone(),
                timestamp: usage_seq,
                run_id: Some(run_id.into()),
                cause: UsageCause::Provider,
                entry_id: Some(usage_entry_id),
                tool_call_id: None,
                attempt: Some(attempt),
                usage: TokenUsage::default(),
            },
        })?;
        Ok(true)
    }
}

pub struct ToolBatchProcedure;

#[derive(Debug, Clone, PartialEq)]
pub enum ToolRecovery {
    Replay(ToolSpec),
    Synthesized(ToolResult),
}

impl ToolBatchProcedure {
    pub(crate) fn start<S: SessionStore>(
        store: &S,
        run_id: &str,
        assistant_entry_id: &str,
        specs: &[ToolSpec],
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let lane = open_lane(store, run_id)?;
        if effects.has_pending_on_lane(&lane.name) {
            return Err(ProcedureError::Invalid(format!(
                "lane {} has an uncommitted mutation",
                lane.name
            )));
        }
        if !store
            .entries()
            .iter()
            .any(|entry| entry.id == assistant_entry_id)
        {
            return Err(ProcedureError::Invalid(
                "assistant entry does not exist".into(),
            ));
        }
        let mut seq = next_seq_with_effects(store, effects);
        for spec in specs {
            if spec.call_id.trim().is_empty()
                || spec.name.trim().is_empty()
                || spec.result_entry_id.trim().is_empty()
            {
                return Err(ProcedureError::Invalid(
                    "tool identity and result entry id must be non-empty".into(),
                ));
            }
            if let Some(existing) = lane
                .tools
                .iter()
                .find(|tool| tool.run_id == run_id && tool.tool_call_id == spec.call_id)
            {
                if existing.result_entry_id != spec.result_entry_id
                    || existing.tool_name != spec.name
                    || existing.tool_index != spec.index
                {
                    return Err(ProcedureError::Invalid(
                        "tool intent does not match its recorded identity".into(),
                    ));
                }
                continue;
            }
            effects.park(EffectAction::AppendRecord {
                id: format!("tool-intent-action-{run_id}-{}", spec.call_id),
                record: Record::ToolStarted {
                    id: format!("tool-intent-{run_id}-{}", spec.call_id),
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: run_id.into(),
                    assistant_entry_id: assistant_entry_id.into(),
                    tool_index: spec.index,
                    tool_call_id: spec.call_id.clone(),
                    tool_name: spec.name.clone(),
                    effective_args: spec.effective_args.clone(),
                    result_entry_id: spec.result_entry_id.clone(),
                    replay: spec.replay.clone(),
                },
            })?;
            seq += 1;
        }
        Ok(())
    }

    pub(crate) fn finish<S: SessionStore>(
        store: &S,
        run_id: &str,
        result: ToolResult,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::finish_inner(store, run_id, result, Some(TokenUsage::default()), effects)
    }

    pub fn finish_with_usage<S: SessionStore>(
        store: &S,
        run_id: &str,
        result: ToolResult,
        usage: TokenUsage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        Self::finish_inner(store, run_id, result, Some(usage), effects)
    }

    pub fn finish_batch<S: SessionStore>(
        store: &S,
        run_id: &str,
        results: &[ToolResult],
        usage: TokenUsage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        // The caller supplies source order. Each parked result reserves its
        // sequence before the next one is prepared, so a manual drive can
        // release the whole batch without duplicate sequence numbers.
        for result in results {
            Self::finish_inner(store, run_id, result.clone(), Some(usage.clone()), effects)?;
        }
        Ok(())
    }

    pub(crate) fn finish_existing<S: SessionStore>(
        store: &S,
        run_id: &str,
        result: ToolResult,
        usage: Option<TokenUsage>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let lane = open_lane(store, run_id)?;
        let tool = lane
            .tools
            .iter()
            .find(|tool| tool.run_id == run_id && tool.tool_call_id == result.call_id)
            .ok_or_else(|| ProcedureError::Invalid("tool intent does not exist".into()))?;
        if tool.tool_name != result.name {
            return Err(ProcedureError::Invalid(
                "tool result name does not match intent".into(),
            ));
        }
        let usage = usage.unwrap_or_default();
        if tool.completed {
            if store.records().iter().any(|record| {
                matches!(record, Record::Usage {
                    run_id: Some(record_run_id),
                    tool_call_id: Some(record_tool_call_id),
                    cause: UsageCause::Tool,
                    ..
                } if record_run_id == run_id && record_tool_call_id == &result.call_id)
            }) {
                return Ok(());
            }
            let seq = next_seq_with_effects(store, effects);
            effects.park(EffectAction::AppendRecord {
                id: format!("tool-existing-usage-action-{run_id}-{}", result.call_id),
                record: Record::Usage {
                    id: format!("tool-existing-usage-{run_id}-{}", result.call_id),
                    seq,
                    lane: lane.name.clone(),
                    timestamp: seq,
                    run_id: Some(run_id.into()),
                    cause: UsageCause::Tool,
                    entry_id: Some(tool.result_entry_id.clone()),
                    tool_call_id: Some(result.call_id),
                    attempt: None,
                    usage,
                },
            })?;
            return Ok(());
        }
        let entry = store
            .entries()
            .iter()
            .find(|entry| entry.id == tool.result_entry_id)
            .ok_or_else(|| ProcedureError::Invalid("tool result entry does not exist".into()))?;
        if !matches!(&entry.message, AgentMessage::Tool { tool_call_id, name, .. } if tool_call_id == &result.call_id && name == &result.name)
            || entry.terminate != result.terminate
        {
            return Err(ProcedureError::Invalid(
                "persisted tool result does not match intent".into(),
            ));
        }
        let seq = next_seq_with_effects(store, effects);
        effects.park(EffectAction::AppendRecord {
            id: format!("tool-existing-finish-action-{run_id}-{}", result.call_id),
            record: Record::ToolFinished {
                id: format!("tool-existing-finished-{run_id}-{}", result.call_id),
                seq,
                lane: lane.name.clone(),
                timestamp: seq,
                run_id: run_id.into(),
                tool_call_id: result.call_id.clone(),
                result_entry_id: tool.result_entry_id.clone(),
                terminate: result.terminate,
            },
        })?;
        effects.park(EffectAction::AppendRecord {
            id: format!("tool-existing-usage-action-{run_id}-{}", result.call_id),
            record: Record::Usage {
                id: format!("tool-existing-usage-{run_id}-{}", result.call_id),
                seq: seq + 1,
                lane: lane.name.clone(),
                timestamp: seq + 1,
                run_id: Some(run_id.into()),
                cause: UsageCause::Tool,
                entry_id: Some(tool.result_entry_id.clone()),
                tool_call_id: Some(result.call_id),
                attempt: None,
                usage,
            },
        })?;
        Ok(())
    }

    pub(crate) fn finish_existing_batch<S: SessionStore>(
        store: &S,
        run_id: &str,
        results: &[ToolResult],
        usage: TokenUsage,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        for result in results {
            Self::finish_existing(store, run_id, result.clone(), Some(usage.clone()), effects)?;
        }
        Ok(())
    }

    fn finish_inner<S: SessionStore>(
        store: &S,
        run_id: &str,
        result: ToolResult,
        usage: Option<TokenUsage>,
        effects: &mut GatedEffects,
    ) -> Result<(), ProcedureError> {
        let lane = open_lane(store, run_id)?;
        let tool = lane
            .tools
            .iter()
            .find(|tool| tool.run_id == run_id && tool.tool_call_id == result.call_id)
            .ok_or_else(|| ProcedureError::Invalid("tool intent does not exist".into()))?;
        if tool.tool_name != result.name {
            return Err(ProcedureError::Invalid(
                "tool result name does not match intent".into(),
            ));
        }
        if tool.completed {
            return Ok(());
        }
        let entry_seq = next_seq_with_effects(store, effects);
        let call_id = result.call_id.clone();
        let parent_id = if tool.tool_index == 0 {
            tool.assistant_entry_id.clone()
        } else {
            lane.tools
                .iter()
                .find(|candidate| {
                    candidate.run_id == run_id
                        && candidate.assistant_entry_id == tool.assistant_entry_id
                        && candidate.tool_index + 1 == tool.tool_index
                })
                .map(|candidate| candidate.result_entry_id.clone())
                .ok_or_else(|| ProcedureError::Invalid("previous tool result is missing".into()))?
        };
        effects.park(EffectAction::AppendEntry {
            entry: Entry {
                id: tool.result_entry_id.clone(),
                parent_id: Some(parent_id),
                lane: lane.name.clone(),
                seq: entry_seq,
                timestamp: entry_seq,
                message: AgentMessage::Tool {
                    tool_call_id: result.call_id.clone(),
                    name: result.name,
                    content: result.content,
                    is_error: result.is_error,
                    terminate: result.terminate,
                },
                surface_op: super::types::SurfaceOperation::Append,
                terminate: result.terminate,
            },
        })?;
        effects.park(EffectAction::AppendRecord {
            id: format!("tool-finish-action-{run_id}-{}", result.call_id),
            record: Record::ToolFinished {
                id: format!("tool-finished-{run_id}-{}", result.call_id),
                seq: entry_seq + 1,
                lane: lane.name.clone(),
                timestamp: entry_seq + 1,
                run_id: run_id.into(),
                tool_call_id: call_id.clone(),
                result_entry_id: tool.result_entry_id.clone(),
                terminate: result.terminate,
            },
        })?;
        if let Some(usage) = usage {
            effects.park(EffectAction::AppendRecord {
                id: format!("tool-usage-action-{run_id}-{call_id}"),
                record: Record::Usage {
                    id: format!("tool-usage-{run_id}-{call_id}"),
                    seq: entry_seq + 2,
                    lane: lane.name.clone(),
                    timestamp: entry_seq + 2,
                    run_id: Some(run_id.into()),
                    cause: UsageCause::Tool,
                    entry_id: Some(tool.result_entry_id.clone()),
                    tool_call_id: Some(call_id),
                    attempt: None,
                    usage,
                },
            })?;
        }
        Ok(())
    }

    pub fn resume<S: SessionStore>(
        store: &S,
        run_id: &str,
        assistant_entry_id: &str,
        current_specs: &[ToolSpec],
        effects: &mut GatedEffects,
    ) -> Result<Vec<ToolRecovery>, ProcedureError> {
        let lane = open_lane(store, run_id)?;
        if !store
            .entries()
            .iter()
            .any(|entry| entry.id == assistant_entry_id)
        {
            return Err(ProcedureError::Invalid(
                "assistant entry does not exist".into(),
            ));
        }
        let mut seq = next_seq_with_effects(store, effects);
        let mut recoveries = Vec::new();
        for spec in current_specs {
            let tool = lane
                .tools
                .iter()
                .find(|tool| tool.run_id == run_id && tool.tool_call_id == spec.call_id)
                .ok_or_else(|| ProcedureError::Invalid("tool intent does not exist".into()))?;
            if tool.completed {
                continue;
            }
            if tool.tool_name != spec.name
                || tool.tool_index != spec.index
                || tool.result_entry_id != spec.result_entry_id
                || tool.assistant_entry_id != assistant_entry_id
            {
                return Err(ProcedureError::Invalid(
                    "current tool declaration does not match intent".into(),
                ));
            }
            if tool.replay == super::types::ToolReplaySafety::Safe
                && spec.replay == super::types::ToolReplaySafety::Safe
            {
                effects.park(EffectAction::AppendRecord {
                    id: format!("tool-replay-usage-action-{run_id}-{}", spec.call_id),
                    record: Record::Usage {
                        id: format!("tool-replay-usage-{run_id}-{}", spec.call_id),
                        seq,
                        lane: lane.name.clone(),
                        timestamp: seq,
                        run_id: Some(run_id.into()),
                        cause: UsageCause::Replay,
                        entry_id: Some(tool.result_entry_id.clone()),
                        tool_call_id: Some(spec.call_id.clone()),
                        attempt: None,
                        usage: TokenUsage::default(),
                    },
                })?;
                seq += 1;
                recoveries.push(ToolRecovery::Replay(spec.clone()));
                continue;
            }
            let tool_was_started = store.records().iter().any(|record| {
                if let Record::ToolExecutionObserved {
                    tool_call_id,
                    phase,
                    ..
                } = record
                {
                    tool_call_id.as_str() == spec.call_id
                        && *phase == super::types::ToolExecutionPhase::Started
                } else {
                    false
                }
            });
            let content = if tool_was_started {
                "Tool execution started but was interrupted before completion (outcome unknown)."
                    .into()
            } else {
                "Tool execution was never started before interruption.".into()
            };
            let result = ToolResult {
                call_id: spec.call_id.clone(),
                name: spec.name.clone(),
                content,
                is_error: true,
                terminate: false,
            };
            effects.park(EffectAction::AppendEntry {
                entry: Entry {
                    id: tool.result_entry_id.clone(),
                    parent_id: Some(assistant_entry_id.into()),
                    lane: lane.name.clone(),
                    seq,
                    timestamp: seq,
                    message: AgentMessage::Tool {
                        tool_call_id: result.call_id.clone(),
                        name: result.name.clone(),
                        content: result.content.clone(),
                        is_error: result.is_error,
                        terminate: result.terminate,
                    },
                    surface_op: super::types::SurfaceOperation::Append,
                    terminate: result.terminate,
                },
            })?;
            effects.park(EffectAction::AppendRecord {
                id: format!("tool-recovery-finish-action-{run_id}-{}", spec.call_id),
                record: Record::ToolFinished {
                    id: format!("tool-recovery-finished-{run_id}-{}", spec.call_id),
                    seq: seq + 1,
                    lane: lane.name.clone(),
                    timestamp: seq + 1,
                    run_id: run_id.into(),
                    tool_call_id: spec.call_id.clone(),
                    result_entry_id: tool.result_entry_id.clone(),
                    terminate: false,
                },
            })?;
            seq += 2;
            recoveries.push(ToolRecovery::Synthesized(result));
        }
        Ok(recoveries)
    }
}

fn open_lane<S: SessionStore>(
    store: &S,
    run_id: &str,
) -> Result<super::types::LaneState, ProcedureError> {
    if run_id.trim().is_empty() {
        return Err(ProcedureError::Invalid("run id must be non-empty".into()));
    }
    let reduced = super::Reducer::reduce(store)
        .map_err(|error| ProcedureError::Invalid(error.to_string()))?;
    let lane = reduced
        .lanes
        .iter()
        .find(|lane| lane.open_operation.as_deref() == Some(run_id))
        .ok_or_else(|| ProcedureError::Invalid("operation is not open".into()))?;
    Ok(lane.clone())
}

fn current_attempt<S: SessionStore>(store: &S, run_id: &str) -> u32 {
    highest_attempt(store, run_id).max(1)
}

fn highest_attempt<S: SessionStore>(store: &S, run_id: &str) -> u32 {
    store
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::StepAttempt {
                run_id: record_run_id,
                attempt,
                ..
            }
            | Record::Usage {
                run_id: Some(record_run_id),
                attempt: Some(attempt),
                ..
            } if record_run_id == run_id => Some(*attempt),
            _ => None,
        })
        .chain(store.records().iter().filter_map(|record| match record {
            Record::RetryConsumed {
                run_id: record_run_id,
                attempt,
                ..
            } if record_run_id == run_id => Some(*attempt),
            _ => None,
        }))
        .max()
        .unwrap_or(0)
}

fn next_attempt<S: SessionStore>(store: &S, run_id: &str) -> u32 {
    highest_attempt(store, run_id).saturating_add(1)
}

fn next_seq_with_effects<S: SessionStore>(store: &S, effects: &GatedEffects) -> u64 {
    store
        .next_sequence()
        .saturating_sub(1)
        .max(effects.pending_sequences().max().unwrap_or(0))
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{MemoryStore, Reducer, SessionStore};

    fn open_store(run_id: &str) -> MemoryStore {
        let mut store = MemoryStore::new("session");
        let leaf = store.append_message(
            None,
            AgentMessage::User {
                content: "original".into(),
            },
        );
        let seq = store.next_sequence();
        store.append_record(Record::OperationStarted {
            id: run_id.into(),
            seq,
            lane: "main".into(),
            timestamp: seq,
            source_leaf_id: Some(leaf),
            intent: OperationIntent::Run,
        });
        store
    }

    #[test]
    fn checkpoint_open_run_rejects_idle_and_wrong_run() {
        let idle = MemoryStore::new("idle");
        let mut effects = GatedEffects::new();
        assert!(CompactionProcedure::checkpoint_open_run(
            &idle,
            "main",
            "run",
            "summary",
            &[],
            CompactionReason::AdaptiveBudget,
            &mut effects,
        )
        .is_err());

        let store = open_store("run");
        assert!(CompactionProcedure::checkpoint_open_run(
            &store,
            "main",
            "wrong-run",
            "summary",
            &[],
            CompactionReason::AdaptiveBudget,
            &mut effects,
        )
        .is_err());
    }

    #[test]
    fn checkpoint_open_run_moves_leaf_and_keeps_operation_open() {
        let mut store = open_store("run");
        let mut effects = GatedEffects::new();
        CompactionProcedure::checkpoint_open_run(
            &store,
            "main",
            "run",
            "durable summary",
            &[serde_json::json!({
                "context_id": "ctx-1",
                "path": "src/lib.rs",
                "start_line": null,
                "end_line": null,
                "file_sha256": "abc123",
            })],
            CompactionReason::OverflowRecovery,
            &mut effects,
        )
        .unwrap();
        effects.run_to_completion(&mut store).unwrap();

        let reduced = Reducer::reduce(&store).unwrap();
        let lane = reduced.lane("main").unwrap();
        assert_eq!(lane.open_operation.as_deref(), Some("run"));
        let leaf = lane.leaf_id.as_deref().unwrap();
        assert!(leaf.starts_with("compaction-run-"));
        assert!(matches!(
            store.entries().last().map(|entry| &entry.message),
            Some(AgentMessage::Custom { custom_type, payload })
                if custom_type == "compaction_summary"
                    && payload["checkpoint_kind"] == "overflow_recovery"
                    && payload["context_snapshot_index"][0]["context_id"] == "ctx-1"
        ));
    }
}
