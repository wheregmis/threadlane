use super::*;

impl CodingSessionHarness {
    /// Request abort for all open lanes and return the main lane's run id,
    /// if any.
    pub(crate) fn request_abort(&mut self) -> Result<Option<String>, String> {
        self.cancellation.store(true, Ordering::SeqCst);
        self.ensure_fresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let open_lanes: Vec<(String, String)> = state
            .lanes
            .iter()
            .filter_map(|lane| {
                lane.open_operation
                    .as_ref()
                    .map(|run_id| (lane.name.clone(), run_id.clone()))
            })
            .collect();
        if open_lanes.is_empty() {
            return Ok(None);
        }
        let main_run_id = state
            .lane("main")
            .and_then(|lane| lane.open_operation.clone());
        for (lane_name, run_id) in open_lanes {
            let is_already_requested = state.lane(&lane_name).is_some_and(|l| l.abort_requested);
            if !is_already_requested {
                let _ = self.store.request_abort(&run_id);
                let _ = self.store.drive_to_completion();
            }
        }
        Ok(main_run_id)
    }

    pub(crate) fn observe_abort_signal(
        &mut self,
        run_id: &str,
        acknowledged: bool,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let attempt = state.lane("main").map(|lane| lane.attempts);
        let finished_ids: std::collections::HashSet<&str> = self
            .store
            .records()
            .iter()
            .filter_map(|candidate| match candidate {
                HarnessRecord::ProviderRequestFinished {
                    run_id: finished_run_id,
                    request_id: Some(finished_request_id),
                    ..
                } if finished_run_id == run_id => Some(finished_request_id.as_str()),
                _ => None,
            })
            .collect();
        let unfinished_requests = self
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestStarted {
                    run_id: provider_run_id,
                    attempt,
                    request_id: Some(request_id),
                    ..
                } if provider_run_id == run_id && !finished_ids.contains(request_id.as_str()) => {
                    Some((*attempt, request_id.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (provider_attempt, request_id) in unfinished_requests {
            let seq = harness_next_seq(self.store.store());
            self.store
                .append_record_gated(HarnessRecord::ProviderRequestFinished {
                    id: format!("provider-finish-{run_id}-{}", request_id.as_str()),
                    seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt: provider_attempt,
                    request_id: Some(request_id),
                    outcome: ProviderOutcome::Aborted,
                    error: Some(ProviderErrorSummary {
                        category: ErrorCategory::Cancelled,
                        code: TraceString::new("runtime_abort").ok(),
                        retryable: false,
                    }),
                    duration_ms: None,
                    usage: None,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::AbortObserved {
                id: format!("abort-observed-{run_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                observation: AbortObservation::SignalSent,
                initiator: AbortInitiator::User,
                target: AbortTarget::ActiveRun,
                acknowledged,
                detail: None,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Reconcile an aborted operation: insert abort entry, record, and
    /// finish with `Aborted` outcome.  Returns `true` if recovery produced
    /// a terminal state.
    pub(crate) fn recover_abort(&mut self) -> Result<bool, String> {
        self.ensure_fresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let Some(lane) = state.lane("main") else {
            return Ok(false);
        };
        let Some(run_id) = lane.open_operation.clone() else {
            return Ok(false);
        };
        if !lane.abort_requested {
            self.store
                .request_abort(&run_id)
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        let start_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == &run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        if let Some(assistant_entry_id) = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.seq > start_seq && entry.lane == "main")
            .find_map(|entry| {
                matches!(&entry.message, AgentMessage::Assistant { .. }).then_some(entry.id.clone())
            })
        {
            self.store
                .reconcile_abort(&run_id, &assistant_entry_id)
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            return Ok(true);
        }
        let result_entry_id = self.store.records().iter().rev().find_map(|record| {
            matches!(record, HarnessRecord::StepAttempt { run_id: record_run_id, .. } if record_run_id == &run_id)
                .then(|| match record {
                    HarnessRecord::StepAttempt { result_entry_id, .. } => result_entry_id.clone(),
                    _ => unreachable!(),
                })
        });
        let had_result_entry = result_entry_id.is_some();
        let seq_hint = self.next_seq();
        let entry_id =
            result_entry_id.unwrap_or_else(|| format!("abort-entry-{run_id}-{seq_hint}"));
        let has_abort_entry = self.store.entries().iter().any(|entry| {
            entry.id == entry_id
                && matches!(
                    &entry.message,
                    AgentMessage::Assistant {
                        stop_reason: Some(reason),
                        ..
                    } if reason == "aborted"
                )
        });
        if !had_result_entry && !has_abort_entry {
            let attempt_seq = self.next_seq();
            self.store
                .append_record_gated(HarnessRecord::StepAttempt {
                    id: format!("abort-attempt-{run_id}-{attempt_seq}"),
                    seq: attempt_seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.clone(),
                    attempt: lane.attempts.saturating_add(1),
                    result_entry_id: entry_id.clone(),
                    compaction_reason: None,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        if !has_abort_entry {
            let seq = self.next_seq();
            self.store
                .append_entry_gated(HarnessEntry {
                    id: entry_id.clone(),
                    parent_id: lane.leaf_id.clone(),
                    lane: "main".into(),
                    seq,
                    timestamp: timestamp(),
                    message: AgentMessage::Assistant {
                        content: Some("Run aborted before completion.".into()),
                        tool_calls: None,
                        stop_reason: Some("aborted".into()),
                        deferred_handle: None,
                    },
                    surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                    terminate: false,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        self.finish_run(
            &run_id,
            OperationOutcome::Aborted,
            Some("Generation cancelled".into()),
        )?;
        Ok(true)
    }
}
