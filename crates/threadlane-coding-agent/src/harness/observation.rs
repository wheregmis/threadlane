use super::*;

impl CodingSessionHarness {
    // ── Observation ───────────────────────────────────────────────────

    /// Take a point-in-time snapshot of the session.
    pub fn snapshot(&mut self) -> Result<Snapshot, String> {
        self.ensure_fresh()?;
        self.store.snapshot().map_err(|error| error.to_string())
    }

    /// Subscribe to session-scoped events.
    pub fn watch(&mut self) -> Result<HarnessWatch, String> {
        self.ensure_fresh()?;
        let subscription = self
            .store
            .watch_session()
            .map_err(|error| error.to_string())?;
        Ok(HarnessWatch {
            hub: self.events.clone(),
            subscription,
        })
    }

    /// Drive all pending effects to completion.
    pub fn drive_to_completion(&mut self) -> Result<(), String> {
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Internal ──────────────────────────────────────────────────────

    /// Re-read the store from disk to pick up external writes.
    pub fn refresh(&mut self) -> Result<(), String> {
        self.ensure_fresh()
    }

    pub(super) fn next_seq(&self) -> u64 {
        self.store.store().next_sequence()
    }

    /// Legacy test/repair helper for journal-free or recovery-only callers.
    ///
    /// Production provider execution must commit typed transitions through the
    /// run-scoped recorders before the next request; it must not reconcile a
    /// complete mutable provider transcript after the fact.
    #[cfg(test)]
    pub fn sync_messages(&mut self, messages: &[AgentMessage]) -> Result<(), String> {
        self.ensure_fresh()?;
        // The provider gives us the complete conversation, not stable entry
        // IDs.  Track occurrences rather than using a set: two turns can
        // legitimately produce byte-for-byte identical assistant messages,
        // including an empty assistant result.
        let mut existing: HashMap<String, usize> = HashMap::new();
        for entry in self
            .store
            .model_context("main")
            .map_err(|error| error.to_string())?
            .entries
        {
            *existing.entry(format!("{:?}", entry.message)).or_default() += 1;
        }

        for msg in messages {
            if matches!(msg, AgentMessage::System { .. }) {
                continue;
            }
            // Initial prompts are already present through begin_run, while
            // queued/steered/generated user messages may exist only in the
            // provider transcript. Occurrence matching handles both cases.
            let key = format!("{:?}", msg);
            if let Some(count) = existing.get_mut(&key) {
                if *count > 0 {
                    *count -= 1;
                    continue;
                }
            }
            if let AgentMessage::Tool { tool_call_id, .. } = msg {
                self.ensure_fresh()?;
                let unfinished_tool = Reducer::reduce(self.store.store()).ok().and_then(|state| {
                    let lane = state.lane("main")?;
                    let run_id = lane.open_operation.as_deref()?;
                    lane.tools.iter().find_map(|tool| {
                        (tool.run_id == run_id
                            && tool.tool_call_id == *tool_call_id
                            && !tool.completed)
                            .then(|| (run_id.to_owned(), tool.result_entry_id.clone()))
                    })
                });
                if let Some((run_id, result_entry_id)) = unfinished_tool {
                    // ToolStarted may be durable while its result entry is
                    // not, if the process was interrupted between those
                    // writes. Recreate the entry before closing the intent.
                    if !self
                        .store
                        .entries()
                        .iter()
                        .any(|entry| entry.id == result_entry_id)
                    {
                        self.append_synced_message(msg.clone())?;
                    }
                    self.finish_tool_message(&run_id, msg)?;
                    continue;
                }
            }
            self.append_synced_message(msg.clone())?;
        }
        Ok(())
    }
    pub fn assert_model_visible(&mut self, messages: &[AgentMessage]) -> Result<(), String> {
        self.ensure_fresh()?;
        let logged = self
            .store
            .model_context("main")
            .map_err(|error| error.to_string())?
            .messages();
        let expected = messages
            .iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .cloned()
            .collect::<Vec<_>>();
        if logged == expected {
            return Ok(());
        }
        let mismatch = logged
            .iter()
            .zip(expected.iter())
            .position(|(logged, expected)| logged != expected)
            .unwrap_or_else(|| logged.len().min(expected.len()));
        Err(format!(
            "model-visible history diverges at index {mismatch}: durable_count={}, provider_count={}",
            logged.len(),
            expected.len()
        ))
    }

    pub fn commit_assistant_message(
        &mut self,
        lane: &str,
        run_id: &str,
        content: Option<String>,
        stop_reason: Option<String>,
    ) -> Result<String, String> {
        self.append_message_to_lane(
            lane,
            run_id,
            AgentMessage::Assistant {
                content,
                tool_calls: None,
                stop_reason,
                deferred_handle: None,
            },
        )
    }

    pub fn commit_thinking(
        &mut self,
        lane: &str,
        run_id: &str,
        reasoning: String,
    ) -> Result<String, String> {
        self.append_message_to_lane(
            lane,
            run_id,
            AgentMessage::Custom {
                custom_type: "thinking".to_string(),
                payload: Value::String(reasoning),
            },
        )
    }

    pub fn commit_tool_calls(
        &mut self,
        lane: &str,
        run_id: &str,
        tool_calls: Vec<threadlane_provider::openai::ToolCall>,
    ) -> Result<String, String> {
        self.append_message_to_lane(
            lane,
            run_id,
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(tool_calls),
                stop_reason: None,
                deferred_handle: None,
            },
        )
    }

    pub fn commit_tool_results(
        &mut self,
        lane: &str,
        run_id: &str,
        results: &[AgentToolResult],
    ) -> Result<Vec<String>, String> {
        let mut committed = Vec::new();
        for result in results {
            let msg = AgentMessage::Tool {
                tool_call_id: result.tool_call_id.clone(),
                name: result.name.clone(),
                content: result.content.clone(),
                is_error: result.is_error,
                terminate: result.terminates(),
                images: result.images.clone(),
            };
            let entry_id = self.append_message_to_lane(lane, run_id, msg)?;
            let _ = self.finish_tool_result(run_id, result);
            committed.push(entry_id);
        }
        Ok(committed)
    }

    pub fn commit_follow_up(
        &mut self,
        lane: &str,
        run_id: &str,
        message: AgentMessage,
    ) -> Result<String, String> {
        self.append_message_to_lane(lane, run_id, message)
    }

    pub fn commit_provider_failure(
        &mut self,
        _lane: &str,
        run_id: &str,
        error: String,
    ) -> Result<(), String> {
        self.finish_run(run_id, OperationOutcome::Failed, Some(error))
    }

    pub fn plan_recovery(
        &mut self,
        lane: &str,
    ) -> Result<threadlane_runtime::harness::RecoveryPlan, String> {
        self.ensure_fresh()?;
        let agent = threadlane_runtime::harness::SessionAgent::new(AgentHarness::new(
            self.store.store().clone(),
        ));
        let lane_handle = threadlane_runtime::harness::LaneHandle::new(lane.to_string())
            .map_err(|error| error.to_string())?;
        agent
            .plan_recovery(&lane_handle)
            .map_err(|error| error.to_string())
    }

    /// Run hooks of the given kind for the main lane.
    pub async fn run_hooks(&self, kind: HookKind, context: &HookContext) {
        for failure in self.store.hooks().run(kind, context).await {
            eprintln!(
                "hook {} ({:?}) failed: {}",
                failure.id, kind, failure.message
            );
        }
    }
}
