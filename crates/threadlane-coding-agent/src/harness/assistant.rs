use super::*;

impl CodingSessionHarness {
    // ── Assistant attempt & messages ──────────────────────────────────

    /// Append a user/assistant/tool message as a harness entry on the main
    /// lane.
    ///
    /// Consecutive identical messages are legitimate (e.g. two `"hello"`
    /// user turns), so no last-entry content dedup is applied. Idempotency
    /// for tool results is handled by deterministic entry ids below.
    pub(crate) fn append_message(&mut self, message: AgentMessage) -> Result<String, String> {
        self.append_message_inner(message, false, false)
    }

    /// Append a message discovered while reconciling the provider transcript.
    ///
    /// Transcript synchronization already determines whether this is a new
    /// occurrence.  It must not apply the legacy last-entry content check,
    /// because two consecutive provider messages can legitimately have the
    /// same serialized value.
    #[cfg(test)]
    pub(super) fn append_synced_message(&mut self, message: AgentMessage) -> Result<String, String> {
        self.append_message_inner(message, false, false)
    }

    /// Restores a retained compaction-tail occurrence to model context.
    ///
    /// A replacement with an empty range changes no prior context entries. Its
    /// non-append surface metadata identifies this as context restoration rather
    /// than a second human-visible transcript occurrence.
    pub(crate) fn append_message_occurrence(
        &mut self,
        message: AgentMessage,
    ) -> Result<String, String> {
        self.append_message_inner(message, false, true)
    }

    fn append_message_inner(
        &mut self,
        message: AgentMessage,
        _deduplicate_last_entry: bool,
        context_restoration: bool,
    ) -> Result<String, String> {
        self.ensure_fresh()?;
        // Single reduce + one id set replaces the previous double-reduce
        // and nested `entries.iter().any` scans.
        let reduced = Reducer::reduce(&self.store).ok();
        let main_lane = reduced.as_ref().and_then(|state| state.lane("main"));
        let parent_id = main_lane.and_then(|lane| lane.leaf_id.clone()).or_else(|| {
            self.store
                .entries()
                .iter()
                .rev()
                .find(|entry| entry.lane == "main")
                .map(|entry| entry.id.clone())
        });
        let entry_ids: std::collections::HashSet<&str> = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        let seq = self.next_seq();
        let terminate = matches!(
            &message,
            AgentMessage::Tool {
                terminate: true,
                ..
            }
        );
        let id = match &message {
            AgentMessage::Assistant { .. } => main_lane
                .and_then(|lane| lane.open_operation.clone())
                .and_then(|run_id| {
                    self.store
                        .records()
                        .iter()
                        .rev()
                        .find_map(|record| match record {
                            HarnessRecord::StepAttempt {
                                run_id: record_run_id,
                                result_entry_id,
                                ..
                            } if record_run_id == &run_id
                                && !entry_ids.contains(result_entry_id.as_str()) =>
                            {
                                Some(result_entry_id.clone())
                            }
                            _ => None,
                        })
                })
                .unwrap_or_else(|| format!("v2-entry-{seq}")),
            AgentMessage::Tool { tool_call_id, .. } => {
                // Key by (run, call): the same call id retried with different
                // content (abort replay, provider retry) is a new occurrence
                // with its own entry, never a DuplicateId on the first one.
                let base = match main_lane.and_then(|lane| lane.open_operation.clone()) {
                    Some(run) => format!("v2-tool-result-{run}-{tool_call_id}"),
                    None => format!("v2-tool-result-{tool_call_id}"),
                };
                if entry_ids.contains(base.as_str())
                    && !self
                        .store
                        .entries()
                        .iter()
                        .any(|entry| entry.id == base && entry.message == message)
                {
                    let mut ordinal = 1u32;
                    loop {
                        let candidate = format!("{base}-retry-{ordinal}");
                        if !entry_ids.contains(candidate.as_str()) {
                            break candidate;
                        }
                        ordinal += 1;
                    }
                } else {
                    base
                }
            }
            _ => format!("v2-entry-{seq}"),
        };
        // Tool completions are recorded both by the execution lifecycle and
        // by the model-visible transcript.  They may be separated by other
        // journal records, so checking only the last entry is insufficient.
        if entry_ids.contains(id.as_str())
            && self
                .store
                .entries()
                .iter()
                .any(|entry| entry.id == id && entry.message == message)
        {
            return Ok(id);
        }
        self.store
            .append_entry_gated(HarnessEntry {
                id: id.clone(),
                parent_id,
                lane: "main".into(),
                seq,
                timestamp: timestamp(),
                message,
                surface_op: if context_restoration {
                    threadlane_runtime::harness::SurfaceOperation::Replace {
                        start_seq: seq,
                        end_seq: seq.saturating_sub(1),
                        source_event_seqs: Vec::new(),
                    }
                } else {
                    threadlane_runtime::harness::SurfaceOperation::Append
                },
                terminate,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    /// Append a message to a named lane (used for subagent results).
    pub(crate) fn append_message_to_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        message: AgentMessage,
    ) -> Result<String, String> {
        self.ensure_fresh()?;
        let prefix = format!("subagent-entry-{run_id}-");
        if matches!(
            message,
            AgentMessage::User { .. } | AgentMessage::Assistant { .. }
        ) {
            // Collapse only a consecutive re-append of the identical message
            // (retry idempotency, mirroring the main lane's exact-id check
            // below). An identical message later on is a legitimate new turn
            // (e.g. a repeated revive prompt), never a duplicate.
            if let Some(latest) = self
                .store
                .entries()
                .iter()
                .rev()
                .find(|entry| entry.lane == lane && entry.id.starts_with(&prefix))
            {
                if latest.message == message {
                    return Ok(latest.id.clone());
                }
            }
        }
        let ordinal = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.id.starts_with(&prefix))
            .count();
        let id = match &message {
            AgentMessage::Tool { tool_call_id, .. } => {
                format!("subagent-result-{run_id}-{tool_call_id}")
            }
            AgentMessage::Assistant { .. } => self
                .store
                .records()
                .iter()
                .filter_map(|record| match record {
                    HarnessRecord::StepAttempt {
                        run_id: record_run,
                        result_entry_id,
                        ..
                    } if record_run == run_id => Some(result_entry_id.clone()),
                    _ => None,
                })
                .next()
                .unwrap_or_else(|| format!("{prefix}{ordinal}")),
            _ => format!("{prefix}{ordinal}"),
        };
        if let Some(entry) = self
            .store
            .entries()
            .iter()
            .find(|entry| entry.lane == lane && entry.id == id)
        {
            return Ok(entry.id.clone());
        }
        let parent_id = match &message {
            AgentMessage::Tool { tool_call_id, .. } => self
                .store
                .records()
                .iter()
                .rev()
                .find_map(|record| match record {
                    HarnessRecord::ToolStarted {
                        lane: record_lane,
                        run_id: record_run,
                        tool_call_id: id,
                        assistant_entry_id,
                        ..
                    } if record_lane == lane && record_run == run_id && id == tool_call_id => {
                        Some(assistant_entry_id.clone())
                    }
                    _ => None,
                })
                .or_else(|| {
                    Reducer::reduce(self.store.store())
                        .ok()
                        .and_then(|state| state.lane(lane).and_then(|l| l.leaf_id.clone()))
                }),
            _ => Reducer::reduce(self.store.store())
                .ok()
                .and_then(|state| state.lane(lane).and_then(|l| l.leaf_id.clone()))
                .or_else(|| {
                    self.store
                        .entries()
                        .iter()
                        .rev()
                        .find(|e| e.lane == lane)
                        .map(|e| e.id.clone())
                }),
        };
        let seq = self.next_seq();
        let terminate = matches!(
            &message,
            AgentMessage::Tool {
                terminate: true,
                ..
            }
        );
        let entry = HarnessEntry {
            id: id.clone(),
            seq,
            lane: lane.into(),
            parent_id,
            timestamp: timestamp(),
            message,
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate,
        };
        self.store
            .append_entry_gated(entry)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    /// Prepare an assistant attempt record for the given run.  Returns
    /// the result entry id that the assistant message should carry.
    pub(crate) fn prepare_assistant_attempt(&mut self, run_id: &str) -> Result<String, String> {
        self.ensure_fresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let lane = state
            .lane("main")
            .filter(|lane| lane.open_operation.as_deref() == Some(run_id))
            .ok_or_else(|| format!("harness operation {run_id} is not open"))?;

        if let Some(result_entry_id) = self.store.records().iter().find_map(|record| {
            let HarnessRecord::StepAttempt {
                run_id: record_run_id,
                result_entry_id,
                ..
            } = record
            else {
                return None;
            };
            (record_run_id == run_id
                && !self
                    .store
                    .entries()
                    .iter()
                    .any(|entry| entry.id == *result_entry_id))
            .then(|| result_entry_id.clone())
        }) {
            return Ok(result_entry_id);
        }

        let attempt = lane.attempts.saturating_add(1);
        let result_entry_id = format!("entry-{run_id}-assistant-{attempt}");
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::StepAttempt {
                id: format!("attempt-{run_id}-{attempt}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                result_entry_id: result_entry_id.clone(),
                compaction_reason: None,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(result_entry_id)
    }

    /// Record a completed assistant attempt after the assistant message
    /// has been appended.
    pub(crate) fn record_assistant_attempt(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let start_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        let result_entry_id = self
            .store
            .entries()
            .iter()
            .filter(|entry| {
                entry.seq > start_seq && matches!(&entry.message, AgentMessage::Assistant { .. })
            })
            .max_by_key(|entry| entry.seq)
            .map(|entry| entry.id.clone())
            .ok_or_else(|| format!("run {run_id} has no assistant result"))?;
        self.store
            .finish_assistant_attempt(run_id, &result_entry_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

}
