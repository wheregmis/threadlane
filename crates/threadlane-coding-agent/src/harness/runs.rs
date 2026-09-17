use super::*;

impl CodingSessionHarness {
    pub fn checkpoint(
        &mut self,
        lane: &str,
        run_id: &str,
        messages: &[AgentMessage],
    ) -> Result<(), String> {
        if messages.is_empty() {
            return Ok(());
        }
        self.ensure_fresh()?;
        for message in messages {
            self.append_message_to_lane(lane, run_id, message.clone())?;
        }
        Ok(())
    }

    // ── Run lifecycle ─────────────────────────────────────────────────

    /// Start a foreground operation and accept the user prompt.
    ///
    /// Returns `Ok(AcceptedRun)` after `accept_prompt` is driven to completion
    /// (committed to the JSONL store).
    pub fn begin_run(
        &mut self,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<AcceptedRun, String> {
        self.ensure_fresh()?;
        self.store
            .accept_prompt_and_drive_on_lane(&self.main_lane_name, run_id, prompt)
            .map_err(|error| error.to_string())
    }

    pub fn enqueue_unbound_with_images(
        &mut self,
        queue: QueueKind,
        content: String,
        images: Vec<ImageAttachment>,
    ) -> Result<String, String> {
        self.ensure_fresh()?;
        let id = format!(
            "entry-queue-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let target = ProvisionedEntry::new(&id, None, AgentMessage::user(content, images));
        self.store
            .enqueue_unbound(queue, target)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    pub fn consume_unbound_queue_entry(
        &mut self,
        queue: QueueKind,
        entry_id: &str,
    ) -> Result<Option<AgentMessage>, String> {
        let Some(message) = self.unbound_queue_message(queue, entry_id)? else {
            return Ok(None);
        };
        self.store
            .consume_unbound(entry_id)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(Some(message))
    }

    pub fn unbound_queue_message(
        &mut self,
        queue: QueueKind,
        entry_id: &str,
    ) -> Result<Option<AgentMessage>, String> {
        self.ensure_fresh()?;
        let state = Reducer::reduce(self.store.store())
            .map_err(|error| format!("reduce failed: {error:?}"))?;
        let lane = state
            .lane(&self.main_lane_name)
            .ok_or_else(|| format!("unknown lane: {}", self.main_lane_name))?;
        Ok(lane
            .queued
            .iter()
            .find(|q| q.run_id.is_none() && q.queue == queue && q.target.id == entry_id)
            .map(|queued| queued.target.message.clone()))
    }

    /// Validate an accepted run token against the session journal and reduced state.
    pub fn validate_accepted_run(&self, accepted: &AcceptedRun) -> Result<(), String> {
        self.store
            .validate_accepted_run(accepted)
            .map_err(|error| error.to_string())
    }

    /// Append a tool intent.
    pub async fn append_tool_intent(
        &mut self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        if self.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ToolStarted {
                run_id: record_run_id,
                tool_call_id: record_call_id,
                ..
            } if record_run_id == run_id && record_call_id == tool_call_id)
        }) {
            return Ok(());
        }
        self.run_before_tool_hook(run_id, tool_call_id, tool_name)
            .await?;
        self.append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
            .await
    }

    pub async fn run_before_tool_hook(
        &self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
    ) -> Result<(), String> {
        let context = HookContext {
            session_id: self.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.into()),
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            tool_arguments: None,
            tool_result_content: None,
            tool_result_is_error: None,
        };
        self.store
            .hooks()
            .run_before_tool(&context)
            .await
            .map_err(|failures| {
                failures
                    .into_iter()
                    .map(|failure| {
                        format!(
                            "{} ({tool_call_id}/{tool_name}): {}",
                            failure.id, failure.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
    }

    /// Start a foreground operation with an optional prompt.
    pub fn start(
        &mut self,
        run_id: &str,
        prompt: Option<AgentMessage>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .start_operation(run_id, None, OperationIntent::Run)
            .map_err(|error| error.to_string())?;
        if let Some(msg) = prompt {
            self.store
                .accept_prompt(run_id, msg)
                .map_err(|error| error.to_string())?;
        }
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish an operation with the given outcome and optional error.
    pub fn finish(
        &mut self,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.finish_run(run_id, outcome, error)
    }

    /// Finish an operation with the given outcome and optional error.
    pub fn finish_run(
        &mut self,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.store
            .finish_operation(run_id, outcome, error)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Generate a unique run identifier scoped to this session.
    pub fn unique_run_id(&mut self, prefix: &str) -> Result<String, String> {
        self.ensure_fresh()?;
        let used_ids = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.id.clone())
            .chain(
                self.store
                    .records()
                    .iter()
                    .map(|record| record.id().to_owned()),
            )
            .collect::<Vec<_>>();
        Ok(SessionIdGenerator::new(self.store.session_id()).next(prefix, &used_ids))
    }
}
