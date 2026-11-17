use super::*;

impl CodingSessionHarness {
    // ── Tools ─────────────────────────────────────────────────────────

    pub(crate) fn tool_execution_identity(
        &self,
        run_id: &str,
        tool_call_id: &str,
    ) -> Result<ToolExecutionIdentity, String> {
        let (lane, tool) = self
            .store
            .store()
            .tool_state_for_call(run_id, tool_call_id)
            .ok_or_else(|| format!("missing committed intent for tool {tool_call_id} in run {run_id}"))?;
        Ok(ToolExecutionIdentity {
            session_id: self.store.session_id().into(),
            lane: lane.into(),
            run_id: tool.run_id.clone(),
            assistant_entry_id: tool.assistant_entry_id.clone(),
            tool_call_id: tool.tool_call_id.clone(),
            tool_name: tool.tool_name.clone(),
            result_entry_id: tool.result_entry_id.clone(),
        })
    }

    /// Record a tool intent (after hooks have run).
    pub(crate) async fn append_tool_intent_after_hook(
        &mut self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<ToolExecutionIdentity, String> {
        self.ensure_fresh()?;
        if self.store.store().has_tool_started(run_id, tool_call_id) {
            return self.tool_execution_identity(run_id, tool_call_id);
        }
        let (assistant_id, tool_index) = self
            .store
            .store()
            .assistant_entry_for_run_call(run_id, tool_call_id)
            .and_then(|assistant| match &assistant.message {
                AgentMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } => calls
                    .iter()
                    .position(|call| call.id == tool_call_id)
                    .map(|tool_index| (assistant.id.clone(), tool_index)),
                _ => None,
            })
            .ok_or_else(|| format!("missing assistant entry for tool {tool_call_id}"))?;
        self.store
            .start_tool_batch(
                run_id,
                &assistant_id,
                &[ToolSpec {
                    index: tool_index,
                    call_id: tool_call_id.into(),
                    name: tool_name.into(),
                    effective_args,
                    result_entry_id: format!("v2-tool-result-{run_id}-{tool_call_id}"),
                    replay: match threadlane_runtime::classify_tool_replay_safety(tool_name) {
                        threadlane_runtime::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                        threadlane_runtime::ToolReplaySafety::Never => {
                            HarnessToolReplaySafety::Never
                        }
                    },
                }],
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        self.tool_execution_identity(run_id, tool_call_id)
    }

    /// Record tool-started on a specific lane (subagent support).
    pub(crate) fn tool_started_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        if self.store.store().has_tool_started(run_id, tool_call_id) {
            return Ok(());
        }
        let (operation_lane, start_seq) = self
            .store
            .store()
            .open_operation_lane(run_id)
            .ok_or_else(|| format!("harness operation {run_id} is not open"))?;
        if operation_lane != lane {
            return Err(format!(
                "harness operation {run_id} does not belong to lane {lane}"
            ));
        }
        let result_entry_id = format!("subagent-result-{run_id}-{tool_call_id}");
        // Prefer the assistant entry that actually declares this call: the
        // reducer validates `(call_id, name)` at `calls[tool_index]`, so both
        // the entry and the index must come from the declaration. Counting
        // prior `ToolStarted` records is only correct within a single batch;
        // across turns it points past the end of a one-call declaration and
        // faults with "tool intent does not match assistant declaration".
        let declaring = self
            .store
            .store()
            .assistant_entry_for_run_call(run_id, tool_call_id)
            .map(|entry| entry.id.clone());
        let assistant_entry_id = match declaring {
            Some(id) => id,
            None => match self
                .store
                .store()
                .last_assistant_entry(lane)
                .filter(|entry| entry.seq > start_seq)
                .map(|entry| entry.id.clone())
            {
                Some(id) => id,
                None => {
                    let assistant_msg = AgentMessage::Assistant {
                        content: None,
                        tool_calls: None,
                        stop_reason: None,
                        deferred_handle: None,
                    };
                    self.append_message_to_lane(lane, run_id, assistant_msg)?
                }
            },
        };
        let declared_entry = self.store.store().entry(&assistant_entry_id);
        let tool_index = match declared_entry {
            Some(entry)
                if matches!(
                    &entry.message,
                    AgentMessage::Assistant { tool_calls: Some(calls), .. }
                        if calls.iter().any(|call| call.id == tool_call_id)
                ) =>
            {
                match &entry.message {
                    AgentMessage::Assistant {
                        tool_calls: Some(calls),
                        ..
                    } => calls
                        .iter()
                        .position(|call| call.id == tool_call_id)
                        .unwrap_or(0),
                    _ => 0,
                }
            }
            // No declaring entry (synthesized empty assistant): keep the
            // count-based ordinal so sequential undeclared tools stay unique.
            _ => self
                .store
                .records()
                .iter()
                .filter(|record| match record {
                    HarnessRecord::ToolStarted {
                        run_id: r_id,
                        lane: r_lane,
                        ..
                    } => r_id == run_id && r_lane == lane,
                    _ => false,
                })
                .count(),
        };
        let record = HarnessRecord::ToolStarted {
            id: format!("tool-started-{run_id}-{tool_call_id}"),
            seq: harness_next_seq(self.store.store()),
            lane: lane.into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            assistant_entry_id,
            tool_index,
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            effective_args,
            result_entry_id,
            replay: match threadlane_runtime::classify_tool_replay_safety(tool_name) {
                threadlane_runtime::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                threadlane_runtime::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
            },
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish a tool message: record ToolFinished and drive effects.
    #[cfg(test)]
    pub(crate) fn finish_tool_message(
        &mut self,
        run_id: &str,
        message: &AgentMessage,
    ) -> Result<(), String> {
        let AgentMessage::Tool {
            tool_call_id,
            name,
            content,
            is_error,
            terminate,
            images,
        } = message
        else {
            return Ok(());
        };
        self.ensure_fresh()?;
        self.store
            .finish_existing_tool(
                run_id,
                threadlane_runtime::harness::ToolResult {
                    call_id: tool_call_id.clone(),
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    terminate: *terminate,
                    images: images.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish a freshly executed tool result: record the tool result Entry, ToolFinished, and drive effects.
    pub(crate) fn finish_tool_result(
        &mut self,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .finish_tool(
                run_id,
                HarnessToolResult {
                    call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    terminate: result.terminates(),
                    images: result.images.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Record tool completions with termination flags.
    pub(crate) fn record_completed_tools_with_termination(
        &mut self,
        run_id: &str,
        termination: &HashMap<String, bool>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let (start_seq, lane) = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, lane, .. } if id == run_id => {
                    Some((*seq, lane.clone()))
                }
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        let Some(assistant) = self
            .store
            .entries()
            .iter()
            // Child transcripts share this store but are not part of this run's batch.
            .filter(|entry| entry.lane == lane && entry.seq > start_seq)
            .filter(|entry| {
                matches!(&entry.message,
                AgentMessage::Assistant {
                    tool_calls: Some(tool_calls),
                    ..
                } if !tool_calls.is_empty())
            })
            .max_by_key(|entry| entry.seq)
        else {
            return Ok(());
        };
        let assistant_id = assistant.id.clone();
        let tool_entries = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.lane == lane && entry.seq > assistant.seq)
            .filter_map(|entry| match &entry.message {
                AgentMessage::Tool {
                    tool_call_id, name, ..
                } => Some((tool_call_id.clone(), name.clone(), entry.id.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let AgentMessage::Assistant {
            tool_calls: Some(tool_calls),
            ..
        } = &assistant.message
        else {
            return Ok(());
        };
        let tool_calls = tool_calls.clone();
        if tool_calls
            .iter()
            .any(|call| !tool_entries.iter().any(|(id, _, _)| id == &call.id))
        {
            return Err(format!("run {run_id} has an incomplete tool batch"));
        }
        for (index, call) in tool_calls.iter().enumerate() {
            // Latest occurrence wins: a retried call id carries its newest
            // output in the last entry, not the first.
            let (_, name, result_entry) = tool_entries
                .iter()
                .rev()
                .find(|(id, _, _)| id == &call.id)
                .expect("tool batch completeness was checked");
            let persisted_result = self
                .store
                .entry(result_entry)
                .and_then(|entry| match &entry.message {
                    AgentMessage::Tool {
                        content, is_error, ..
                    } => Some((content.clone(), *is_error)),
                    _ => None,
                })
                .ok_or_else(|| format!("run {run_id} has an invalid tool result"))?;
            let args = serde_json::from_str(&call.function.arguments)
                .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));
            let replay = match threadlane_runtime::classify_tool_replay_safety(name) {
                threadlane_runtime::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                threadlane_runtime::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
            };
            let started = self.store.store().has_tool_started(run_id, &call.id);
            if !started {
                self.store
                    .start_tool_batch(
                        run_id,
                        &assistant_id,
                        &[ToolSpec {
                            index,
                            call_id: call.id.clone(),
                            name: name.to_string(),
                            effective_args: args,
                            result_entry_id: result_entry.clone(),
                            replay,
                        }],
                    )
                    .map_err(|error| error.to_string())?;
                self.store
                    .drive_to_completion()
                    .map_err(|error| error.to_string())?;
            }
            let terminate = termination.get(&call.id).copied().unwrap_or(false);
            self.store
                .finish_existing_tool(
                    run_id,
                    threadlane_runtime::harness::ToolResult {
                        call_id: call.id.clone(),
                        name: name.clone(),
                        content: persisted_result.0,
                        is_error: persisted_result.1,
                        terminate,
                        images: Vec::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
