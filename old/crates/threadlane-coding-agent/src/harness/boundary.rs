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
        // Recaptures (retry / model-switch) must not collide with the first
        // snapshot: suffix the record id by per-run capture ordinal, or the
        // second capture faults with DuplicateId and blocks the retry.
        let capture = self
            .store
            .store()
            .records()
            .iter()
            .filter(|record| {
                matches!(record, HarnessRecord::RunContextCaptured { run_id: record_run, .. } if record_run == run_id)
            })
            .count()
            + 1;
        let record = HarnessRecord::RunContextCaptured {
            id: format!("run-context-{run_id}-{capture}"),
            seq: harness_next_seq(self.store.store()),
            lane: lane.into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            attempt: Some(capture as u32),
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
        read_digests: &HashMap<String, String>,
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
        let mut current = with_system(self.model_context("main")?.into_messages());
        let mut visible =
            self.provider_read_context(&current, request.tool_schema_json.as_deref(), read_digests);
        let pre_tokens = estimate_request_tokens(
            &visible,
            request.tool_schema_json.as_deref(),
            &CompactionParams::from(config),
        );
        if pre_tokens < budget.trigger_tokens && !request.overflow_recovery {
            let mut result = boundary_result(
                visible,
                budget,
                self.compaction_generation(),
                None,
                provider_attempt,
                provider_request_id,
            );
            result.canonical_messages = Some(current);
            return Ok(result);
        }
        let reason = if request.overflow_recovery {
            CompactionReason::OverflowRecovery
        } else {
            CompactionReason::AdaptiveBudget
        };
        // Squeeze oversized aged tool outputs first: when the squeezed
        // request fits under the trigger there is no checkpoint to commit at
        // all. The squeezed view is request-only — the canonical journal and
        // in-memory context keep the full outputs, and each attempt re-derives
        // the reduction. Committing a checkpoint here would also hide verbatim
        // history behind excerpts earlier than necessary.
        //
        // Overflow recovery skips this stage and always commits a checkpoint:
        // the provider rejected the request at its real limit, so a gentler
        // squeeze that only trims the estimated overage is not a reliable
        // recovery.
        if !request.overflow_recovery {
            const KEEP_RECENT_TOOL_TURNS: usize = 3;
            // Results an unchanged-read reference points at stay verbatim:
            // the reference promises the earlier body is inline, and a
            // preview would leave it pointing at an excerpt.
            let anchored_calls: std::collections::HashSet<&str> = visible
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::Tool { content, .. } => content
                        .strip_prefix(super::context::UNCHANGED_READ_REFERENCE_PREFIX)
                        .and_then(|rest| rest.split('.').next()),
                    _ => None,
                })
                .collect();
            let pruned = threadlane_compaction::prune_historical_tool_outputs_preserving(
                &visible,
                KEEP_RECENT_TOOL_TURNS,
                &anchored_calls,
            );
            let pruned_tokens = estimate_request_tokens(
                &pruned,
                request.tool_schema_json.as_deref(),
                &CompactionParams::from(config),
            );
            if pruned_tokens < budget.trigger_tokens {
                let mut result = boundary_result(
                    pruned,
                    budget,
                    self.compaction_generation(),
                    Some(pruned_tokens),
                    provider_attempt,
                    provider_request_id,
                );
                result.canonical_messages = Some(current);
                return Ok(result);
            }
        }
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
            current = with_system(self.model_context("main")?.into_messages());
            visible = self.provider_read_context(
                &current,
                request.tool_schema_json.as_deref(),
                read_digests,
            );
            let post_tokens = estimate_request_tokens(
                &visible,
                request.tool_schema_json.as_deref(),
                &CompactionParams::from(config),
            );
            if post_tokens < budget.trigger_tokens {
                let mut result = boundary_result(
                    visible,
                    budget,
                    self.compaction_generation(),
                    Some(post_tokens),
                    provider_attempt,
                    provider_request_id,
                );
                result.canonical_messages = Some(current);
                return Ok(result);
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
