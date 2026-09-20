use super::*;

impl CodingSessionHarness {
    pub(crate) fn compaction_summary_without_indexed_tool_outputs(
        &self,
        summary: &str,
        compacted_messages: usize,
        config: &AgentConfig,
    ) -> Result<String, String> {
        let omitted_source_entry_ids = self
            .context_snapshots("main")
            .into_iter()
            .map(|snapshot| snapshot.source_entry_id)
            .collect::<Vec<_>>();
        if omitted_source_entry_ids.is_empty() {
            return Ok(summary.to_owned());
        }
        let dropped = self
            .model_context("main")?
            .entries
            .into_iter()
            .filter(|entry| !matches!(entry.message, AgentMessage::System { .. }))
            .take(compacted_messages)
            .collect::<Vec<_>>();
        let params = CompactionParams::from(config);
        let pairs: Vec<(&AgentMessage, bool)> = dropped
            .iter()
            .map(|entry| (&entry.message, omitted_source_entry_ids.contains(&entry.id)))
            .collect();
        Ok(build_checkpoint_omitting_tool_outputs(&pairs, &params))
    }

    pub(crate) fn context_snapshot_index_for_compaction(
        &self,
        compacted_messages: usize,
    ) -> Result<Vec<Value>, String> {
        let dropped_source_entry_ids = self
            .model_context("main")?
            .entries
            .into_iter()
            .filter(|entry| !matches!(entry.message, AgentMessage::System { .. }))
            .take(compacted_messages)
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        Ok(compacted_context_snapshot_index_for_sources(
            &self.context_snapshots("main"),
            &dropped_source_entry_ids,
        ))
    }

    #[cfg(test)]
    pub(crate) fn checkpoint_open_run_compaction(
        &mut self,
        run_id: &str,
        summary: &str,
        reason: CompactionReason,
    ) -> Result<(), String> {
        self.stage_open_run_compaction(run_id, summary, &[], reason)?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    fn stage_open_run_compaction(
        &mut self,
        run_id: &str,
        summary: &str,
        context_snapshot_index: &[Value],
        reason: CompactionReason,
    ) -> Result<(), String> {
        self.store
            .checkpoint_open_run_compaction("main", run_id, summary, context_snapshot_index, reason)
            .map_err(|error| error.to_string())
    }

    pub(super) fn commit_prepared_compaction(
        &mut self,
        parent_run_id: &str,
        model: &str,
        tool_schema_json: Option<&str>,
        config: &AgentConfig,
        budget: ContextBudget,
        reason: CompactionReason,
        prepared: PreparedCompaction,
    ) -> Result<(), String> {
        let result = (|| {
            let summary = prepared
                .messages
                .iter()
                .find_map(threadlane_compaction::compaction_summary_text)
                .ok_or_else(|| "context preparation produced no durable summary".to_string())?;
            let summary = self.compaction_summary_without_indexed_tool_outputs(
                summary,
                prepared.compacted_messages,
                config,
            )?;
            let context_snapshot_index =
                self.context_snapshot_index_for_compaction(prepared.compacted_messages)?;
            let mut messages = prepared.messages.clone();
            let Some(AgentMessage::Custom { payload, .. }) = messages
                .iter_mut()
                .find(|message| threadlane_compaction::compaction_summary_text(message).is_some())
            else {
                return Err("context preparation produced no durable summary".into());
            };
            payload["summary"] = Value::String(summary.clone());
            payload["context_snapshot_index"] = Value::Array(context_snapshot_index.clone());
            let first_seq = self.next_seq();
            let summary_id = format!("compaction-{parent_run_id}-{first_seq}-summary");
            self.stage_open_run_compaction(
                parent_run_id,
                &summary,
                &context_snapshot_index,
                reason,
            )?;

            let retained = crate::durable::compaction_retained_tail(&messages);
            let mut parent_id = summary_id;
            for (index, message) in retained.into_iter().enumerate() {
                let id = format!("compaction-{parent_run_id}-{first_seq}-tail-{index}");
                let terminate = matches!(
                    &message,
                    AgentMessage::Tool {
                        terminate: true,
                        ..
                    }
                );
                self.store
                    .append_entry_gated(HarnessEntry {
                        id: id.clone(),
                        parent_id: Some(parent_id),
                        lane: "main".into(),
                        seq: first_seq + 2 + index as u64,
                        timestamp: timestamp(),
                        message,
                        surface_op: threadlane_runtime::harness::SurfaceOperation::Replace {
                            start_seq: first_seq + 2 + index as u64,
                            end_seq: (first_seq + 1 + index as u64),
                            source_event_seqs: Vec::new(),
                        },
                        terminate,
                    })
                    .map_err(|error| error.to_string())?;
                parent_id = id;
            }

            let post_tokens = estimate_request_tokens(
                &messages,
                tool_schema_json,
                &CompactionParams::from(config),
            );
            let generation = self.compaction_generation().saturating_add(1);
            let record = HarnessRecord::ContextCompacted {
                id: format!("context-compacted-{parent_run_id}-{generation}"),
                seq: first_seq + 2 + prepared.messages.len() as u64,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: parent_run_id.into(),
                generation,
                reason,
                effective_model: TraceString::new(model)?,
                context_limit: budget.limit,
                context_limit_is_estimate: budget.limit_is_estimate,
                pre_tokens: prepared.pre_tokens,
                post_tokens,
                retained_tail_target: prepared.retained_tail_target,
                retained_tail_tokens: prepared.retained_tail_tokens,
                compacted_messages: prepared.compacted_messages,
            };
            self.store
                .append_record_gated(record)
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion_atomically()
                .map_err(|error| error.to_string())
        })();
        if result.is_err() {
            let _ = self.ensure_fresh();
        }
        result
    }

    pub(crate) fn record_manual_compaction(
        &mut self,
        run_id: &str,
        model: &str,
        config: &AgentConfig,
        pre_tokens: usize,
        retained_tail_tokens: usize,
        compacted_messages: usize,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let budget = context_budget(model, &BudgetConfig::from(config));
        let messages = self.model_context("main")?.messages();
        let post_tokens = estimate_request_tokens(&messages, None, &CompactionParams::from(config));
        let generation = self.compaction_generation().saturating_add(1);
        let record = HarnessRecord::ContextCompacted {
            id: format!("context-compacted-{run_id}-{generation}"),
            seq: self.next_seq(),
            lane: "main".into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            generation,
            reason: CompactionReason::Manual,
            effective_model: TraceString::new(model)?,
            context_limit: budget.limit,
            context_limit_is_estimate: budget.limit_is_estimate,
            pre_tokens,
            post_tokens,
            retained_tail_target: budget.retained_tail_tokens,
            retained_tail_tokens,
            compacted_messages,
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }
}
