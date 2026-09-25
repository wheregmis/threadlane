use super::*;

impl CodingSessionHarness {
    #[cfg(test)]
    pub(crate) fn transcript(
        &self,
        lane: &str,
    ) -> threadlane_runtime::harness::TranscriptProjection {
        self.store.store().transcript(lane)
    }

    #[cfg(test)]
    pub(crate) fn record_provider_trace_to_path(
        path: &Path,
        run_id: &str,
        event: ProviderTraceEvent,
    ) -> Result<(), String> {
        Self::with_path(path, |journal| journal.record_provider_trace(run_id, event))
    }

    pub(crate) fn record_provider_trace(
        &mut self,
        run_id: &str,
        event: ProviderTraceEvent,
    ) -> Result<(), String> {
        self.record_provider_trace_on_lane("main", run_id, event)
    }

    pub(crate) fn record_provider_trace_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        event: ProviderTraceEvent,
    ) -> Result<(), String> {
        // Child responses are persisted by the child checkpoint stream.
        if lane != "main" && matches!(&event, ProviderTraceEvent::AssistantReady { .. }) {
            return Ok(());
        }
        let journal = self;
        let event = match event {
            ProviderTraceEvent::AssistantReady {
                attempt,
                request_id,
                reasoning,
                message,
            } => {
                let reasoning_entry_id = if let Some(reasoning) =
                    reasoning.filter(|reasoning| !reasoning.trim().is_empty())
                {
                    let thinking = AgentMessage::Custom {
                        custom_type: "thinking".into(),
                        payload: serde_json::json!({ "text": reasoning }),
                    };
                    let existing = journal
                        .store
                        .entries()
                        .iter()
                        .rev()
                        .find(|entry| entry.lane == lane && entry.message == thinking)
                        .map(|entry| entry.id.clone());
                    Some(match existing {
                        Some(id) => id,
                        None => journal.append_message_to_lane(lane, run_id, thinking)?,
                    })
                } else {
                    None
                };
                let existing = journal
                    .store
                    .entries()
                    .iter()
                    .rev()
                    .find(|entry| entry.lane == lane && entry.message == message)
                    .map(|entry| entry.id.clone());
                let entry_id = match existing {
                    Some(id) => id,
                    None => journal.append_message_to_lane(lane, run_id, message)?,
                };
                let seq = harness_next_seq(journal.store.store());
                let record = HarnessRecord::ProviderResponseAttached {
                    id: format!("provider-response-{run_id}-{request_id}"),
                    seq,
                    lane: lane.into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt,
                    request_id: Some(TraceString::new(request_id)?),
                    entry_id,
                    reasoning_entry_id,
                };
                journal
                    .store
                    .append_record_gated(record)
                    .map_err(|error| error.to_string())?;
                journal
                    .store
                    .drive_to_completion()
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
            event => event,
        };
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            ProviderTraceEvent::AssistantReady { .. } => unreachable!(),
            ProviderTraceEvent::Started {
                attempt,
                request_id,
                model,
                provider,
            } => HarnessRecord::ProviderRequestStarted {
                id: format!("provider-start-{run_id}-{request_id}"),
                seq,
                lane: lane.into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                provider: TraceString::new(provider)?,
                model: TraceString::new(model)?,
                request_id: Some(TraceString::new(request_id)?),
            },
            ProviderTraceEvent::ContextManifest {
                attempt,
                request_id,
                model,
                context_limit,
                context_limit_is_estimate,
                compaction_generation,
                total_estimated_tokens,
                items,
            } => HarnessRecord::ContextManifestCaptured {
                id: format!("context-manifest-{run_id}-{request_id}"),
                seq,
                lane: lane.into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                request_id: TraceString::new(request_id)?,
                total_estimated_tokens,
                effective_model: Some(TraceString::new(model)?),
                context_limit,
                context_limit_is_estimate,
                compaction_generation,
                items,
            },
            ProviderTraceEvent::Checkpoint {
                attempt,
                request_id,
                checkpoint_index,
                text,
                reasoning,
            } => {
                let mut digest = Sha256::new();
                digest.update(text.as_bytes());
                if let Some(reasoning) = reasoning.as_deref() {
                    digest.update(reasoning.as_bytes());
                }
                HarnessRecord::StreamCheckpoint {
                    id: format!("stream-checkpoint-{run_id}-{request_id}-{checkpoint_index}"),
                    seq,
                    lane: lane.into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt: Some(attempt),
                    request_id: TraceString::new(request_id)?,
                    assistant_entry_id: None,
                    text: (!text.is_empty()).then(|| BoundedText::truncated(&text)),
                    reasoning: reasoning
                        .as_deref()
                        .filter(|reasoning| !reasoning.is_empty())
                        .map(BoundedText::truncated),
                    checkpoint_index,
                    byte_count: text.len() as u64
                        + reasoning.as_ref().map_or(0, String::len) as u64,
                    fingerprint: TraceString::new(format!("{:x}", digest.finalize()))?,
                }
            }
            ProviderTraceEvent::Finished {
                attempt,
                request_id,
                outcome,
                error,
                duration_ms,
                usage,
            } => HarnessRecord::ProviderRequestFinished {
                id: format!("provider-finish-{run_id}-{request_id}"),
                seq,
                lane: lane.into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                request_id: Some(TraceString::new(request_id)?),
                outcome,
                error,
                duration_ms: Some(duration_ms),
                usage,
            },
        };
        // Append through the journal already open above instead of reopening
        // the file; gated append re-checks freshness under the writer gate.
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn record_permission_trace(
        &mut self,
        run_id: Option<&str>,
        event: PermissionTraceEvent,
    ) -> Result<(), String> {
        let journal = self;
        let state = Reducer::reduce(journal.store.store()).map_err(|error| error.to_string())?;
        let attempt = run_id.and_then(|_| state.lane("main").map(|lane| lane.attempts));
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            PermissionTraceEvent::Requested {
                request_id,
                capability,
                scopes,
                detail_sha256,
                source,
            } => HarnessRecord::PermissionRequested {
                id: format!("permission-request-{request_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.map(str::to_owned),
                attempt,
                request_id: TraceString::new(request_id)?,
                capability: TraceString::new(capability)?,
                scopes,
                detail_sha256: TraceString::new(detail_sha256)?,
                source,
            },
            PermissionTraceEvent::Resolved {
                request_id,
                decision,
                scope,
                source,
                remembered,
            } => HarnessRecord::PermissionResolved {
                id: format!("permission-resolved-{request_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.map(str::to_owned),
                attempt,
                request_id: TraceString::new(request_id)?,
                decision,
                scope,
                source,
                remembered,
            },
        };
        // Append through the already-open journal; see record_provider_trace_to_path.
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    pub(crate) async fn record_tool_execution_to_path(
        path: &Path,
        run_id: &str,
        event: ToolExecutionTraceEvent,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.record_tool_execution(run_id, event).await
    }

    pub(crate) async fn record_tool_execution(
        &mut self,
        run_id: &str,
        event: ToolExecutionTraceEvent,
    ) -> Result<(), String> {
        let journal = self;
        if let ToolExecutionTraceEvent::Started {
            tool_call_id,
            tool_name,
            effective_arguments,
            ..
        } = &event
        {
            let has_intent = journal.store.records().iter().any(|record| {
                matches!(
                    record,
                    HarnessRecord::ToolStarted {
                        run_id: intent_run_id,
                        tool_call_id: intent_call_id,
                        ..
                    } if intent_run_id == run_id && intent_call_id == tool_call_id
                )
            });
            if !has_intent {
                let effective_args = serde_json::from_str(effective_arguments)
                    .unwrap_or_else(|_| Value::String(effective_arguments.clone()));
                journal
                    .append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
                    .await?;
            }
        }
        let state = Reducer::reduce(journal.store.store()).map_err(|error| error.to_string())?;
        let attempt = state.lane("main").map(|lane| lane.attempts);
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            ToolExecutionTraceEvent::Started {
                tool_call_id,
                tool_name,
                executor_kind,
                effective_arguments: _,
                started_at_ms,
            } => HarnessRecord::ToolExecutionObserved {
                id: format!("tool-execution-start-{run_id}-{tool_call_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                tool_call_id: TraceString::new(tool_call_id)?,
                tool_name: TraceString::new(tool_name)?,
                executor_kind: TraceString::new(executor_kind)?,
                phase: ToolExecutionPhase::Started,
                started_at_ms: Some(started_at_ms),
                duration_ms: None,
                outcome: None,
                exit_code: None,
                cancelled: false,
                is_error: None,
                terminate: None,
                output_sha256: None,
                output_bytes: None,
            },
            ToolExecutionTraceEvent::Finished {
                tool_call_id,
                tool_name,
                executor_kind,
                started_at_ms,
                duration_ms,
                is_error,
                terminate,
                output_sha256,
                output_bytes,
            } => HarnessRecord::ToolExecutionObserved {
                id: format!("tool-execution-finish-{run_id}-{tool_call_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                tool_call_id: TraceString::new(tool_call_id)?,
                tool_name: TraceString::new(tool_name)?,
                executor_kind: TraceString::new(executor_kind)?,
                phase: ToolExecutionPhase::Finished,
                started_at_ms: Some(started_at_ms),
                duration_ms: Some(duration_ms),
                outcome: Some(if is_error {
                    ToolExecutionOutcome::Failed
                } else {
                    ToolExecutionOutcome::Succeeded
                }),
                exit_code: None,
                cancelled: false,
                is_error: Some(is_error),
                terminate: Some(terminate),
                output_sha256: Some(TraceString::new(output_sha256)?),
                output_bytes: Some(output_bytes),
            },
        };
        // Append through the already-open journal; see record_provider_trace_to_path.
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub async fn append_tool_intent_to_path(
        path: &Path,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal
            .append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn record_tool_result_to_path(
        path: &Path,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        let path = path.to_path_buf();
        let run_id = run_id.to_owned();
        let result = result.clone();
        tokio::task::spawn_blocking(move || {
            Self::with_path(&path, |journal| {
                journal.finish_tool_result(&run_id, &result)
            })
        })
        .await
        .map_err(|error| error.to_string())?
    }

    pub(crate) fn record_tool_result(
        &mut self,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        self.finish_tool_result(run_id, result)
    }
}
