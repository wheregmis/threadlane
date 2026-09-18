use super::*;

impl CodingSessionHarness {
    pub(crate) fn capture_run_context(
        &mut self,
        run_id: &str,
        lane: &str,
        model: String,
        provider: String,
        reasoning_effort: ReasoningEffort,
        prompt_cache_enabled: bool,
        work_dir: String,
        system_prompt: PromptSnapshot,
        tool_schema_sha256: String,
        enabled_tool_names: Vec<String>,
        capabilities: Vec<String>,
        capability_sha256: Option<String>,
        prompt_template_ids: Vec<String>,
        git_head: Option<String>,
        context_window_limit: Option<usize>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let trace = |value: String| TraceString::new(value);
        let record = HarnessRecord::RunContextCaptured {
            id: format!("run-context-{run_id}"),
            seq: harness_next_seq(self.store.store()),
            lane: lane.into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            attempt: None,
            model: trace(model)?,
            provider: trace(provider)?,
            reasoning_effort,
            prompt_cache_enabled,
            work_dir: trace(work_dir)?,
            system_prompt,
            tool_schema_sha256: trace(tool_schema_sha256)?,
            enabled_tool_names: enabled_tool_names
                .into_iter()
                .take(256)
                .map(TraceString::new)
                .collect::<Result<Vec<_>, _>>()?,
            capabilities: CapabilitySnapshot {
                capabilities: capabilities
                    .into_iter()
                    .take(256)
                    .map(TraceString::new)
                    .collect::<Result<Vec<_>, _>>()?,
                fingerprint: capability_sha256.map(TraceString::new).transpose()?,
            },
            prompt_template_ids: prompt_template_ids
                .into_iter()
                .take(256)
                .map(TraceString::new)
                .collect::<Result<Vec<_>, _>>()?,
            git_head: git_head.map(TraceString::new).transpose()?,
            context_window_limit,
            route_defaults: None,
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn model_context(
        &self,
        lane: &str,
    ) -> Result<threadlane_runtime::harness::ModelContextProjection, String> {
        self.store
            .store()
            .model_context(lane)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn prepare_provider_boundary(
        &mut self,
        run_id: &str,
        request: ProviderBoundaryRequest,
        config: &AgentConfig,
    ) -> Result<ProviderBoundaryResult, String> {
        self.ensure_fresh()?;
        // No provider boundary may proceed after cancellation, even when the
        // already-compacted context is below the adaptive trigger. Once a
        // checkpoint procedure starts, it is driven atomically to completion.
        if self.cancellation.load(Ordering::SeqCst) {
            return Err("context preparation cancelled".into());
        }
        let budget = context_budget(&request.model, &BudgetConfig::from(config));
        let provider_attempt = self
            .store
            .store()
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestStarted {
                    run_id: record_run_id,
                    attempt,
                    ..
                } if record_run_id == run_id => Some(*attempt),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let provider_request_id = format!("provider-request-{run_id}-{provider_attempt}");
        // System instructions are runtime configuration, not transcript entries.
        // Reattach them after every durable projection, including compaction reloads.
        let with_system = |messages: Vec<AgentMessage>| {
            request
                .messages
                .iter()
                .filter(|message| matches!(message, AgentMessage::System { .. }))
                .cloned()
                .chain(
                    messages
                        .into_iter()
                        .filter(|message| !matches!(message, AgentMessage::System { .. })),
                )
                .collect::<Vec<_>>()
        };
        let mut current = with_system(self.model_context("main")?.messages());
        let pre_tokens =
            estimate_request_tokens(
                &current,
                request.tool_schema_json.as_deref(),
                &CompactionParams::from(config),
            );
        if pre_tokens < budget.trigger_tokens && !request.overflow_recovery {
            return Ok(boundary_result(
                current,
                budget,
                self.compaction_generation(),
                None,
                provider_attempt,
                provider_request_id,
            ));
        }
        let reason = if request.overflow_recovery {
            CompactionReason::OverflowRecovery
        } else {
            CompactionReason::AdaptiveBudget
        };
        let targets = [
            budget.retained_tail_tokens,
            budget.strict_retained_tail_tokens,
        ];
        for (index, target) in targets.into_iter().enumerate() {
            let Some(prepared) = compact_for_budget(
                &current,
                request.tool_schema_json.as_deref(),
                target,
                &CompactionParams::from(config),
            ) else {
                return Err("context preparation could not drop historical messages".into());
            };
            self.commit_prepared_compaction(
                run_id,
                &request.model,
                request.tool_schema_json.as_deref(),
                config,
                budget,
                reason,
                prepared,
            )?;
            current = with_system(self.model_context("main")?.messages());
            let post_tokens =
                estimate_request_tokens(
                &current,
                request.tool_schema_json.as_deref(),
                &CompactionParams::from(config),
            );
            if post_tokens < budget.trigger_tokens {
                return Ok(boundary_result(
                    current,
                    budget,
                    self.compaction_generation(),
                    Some(post_tokens),
                    provider_attempt,
                    provider_request_id,
                ));
            }
            if index == 1 {
                return Err(format!(
                    "context remains above budget after strict compaction: {post_tokens}/{}",
                    budget.trigger_tokens,
                ));
            }
            self.ensure_fresh()?;
        }
        unreachable!()
    }

    pub(super) fn compaction_generation(&self) -> u64 {
        self.store
            .store()
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ContextCompacted {
                    lane, generation, ..
                } if lane == "main" => Some(*generation),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }
}
