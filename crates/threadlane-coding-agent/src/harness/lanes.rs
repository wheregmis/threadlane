use super::*;

impl CodingSessionHarness {
    pub fn start_subagent_lane(
        &mut self,
        lane_hint: &str,
        task: &str,
        source_leaf_id: Option<&str>,
    ) -> Result<StartedSubagentLane, SubagentStartError> {
        if self.cancellation.load(Ordering::SeqCst) {
            return Err(SubagentStartError {
                identity: None,
                error: "Subagent start rejected because the parent is cancelling".into(),
            });
        }
        static START_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _start_lock = START_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .map_err(|error| SubagentStartError {
                identity: None,
                error: error.to_string(),
            })?;
        let mut attempt_idx = 0;
        let identity = loop {
            self.ensure_fresh().map_err(|error| SubagentStartError {
                identity: None,
                error: error.to_string(),
            })?;
            let used_ids = self
                .store
                .entries()
                .iter()
                .map(|entry| entry.id.clone())
                .chain(
                    self.store
                        .records()
                        .iter()
                        .flat_map(|record| [record.id().to_owned(), record.lane().to_owned()]),
                )
                .collect::<Vec<_>>();
            let generator = SessionIdGenerator::new(self.store.session_id());
            let base_run_id = generator.next("subagent-run", &used_ids);
            let run_id = if attempt_idx == 0 {
                base_run_id
            } else {
                format!("{base_run_id}-{attempt_idx}")
            };
            let mut lane_ids = used_ids.clone();
            lane_ids.push(run_id.clone());
            let base_lane = generator.next(lane_hint, &lane_ids);
            let lane_name = if attempt_idx == 0 {
                base_lane
            } else {
                format!("{base_lane}-{attempt_idx}")
            };
            let mut identity = SubagentLaneIdentity {
                lane_name: lane_name.clone(),
                run_id: run_id.clone(),
                source_leaf_id: source_leaf_id.map(str::to_owned),
                started_seq: 0,
            };
            if let Err(error) = self.store.start_operation_on_lane(
                &lane_name,
                &run_id,
                source_leaf_id.map(str::to_owned),
                OperationIntent::Run,
            ) {
                let err_str = error.to_string();
                if err_str.contains("DuplicateId") {
                    attempt_idx += 1;
                    continue;
                }
                if source_leaf_id.is_some()
                    && (err_str.contains("source leaf does not exist")
                        || err_str.contains("MissingParent"))
                {
                    if let Err(retry_err) = self.store.start_operation_on_lane(
                        &lane_name,
                        &run_id,
                        None,
                        OperationIntent::Run,
                    ) {
                        if retry_err.to_string().contains("DuplicateId") {
                            attempt_idx += 1;
                            continue;
                        }
                        return Err(SubagentStartError {
                            identity: None,
                            error: retry_err.to_string(),
                        });
                    }
                    identity.source_leaf_id = None;
                } else {
                    return Err(SubagentStartError {
                        identity: None,
                        error: err_str,
                    });
                }
            }
            break identity;
        };
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        let prompt_message = AgentMessage::user(task.to_owned(), Vec::new());
        let prompt_entry_id = format!("entry-{}-user", identity.run_id);
        let effective_parent_id = source_leaf_id
            .filter(|id| self.store.entries().iter().any(|e| e.id == *id))
            .map(str::to_owned);
        self.store
            .append_entry_gated(HarnessEntry {
                id: prompt_entry_id,
                parent_id: effective_parent_id,
                lane: identity.lane_name.clone(),
                seq: harness_next_seq(self.store.store()),
                timestamp: timestamp(),
                message: prompt_message,
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .append_record_gated(HarnessRecord::StepAttempt {
                id: format!("assistant-attempt-action-{}-1", identity.run_id),
                seq: harness_next_seq(self.store.store()),
                lane: identity.lane_name.clone(),
                timestamp: timestamp(),
                run_id: identity.run_id.clone(),
                attempt: 1,
                result_entry_id: format!("entry-{}-assistant-1", identity.run_id),
                compaction_reason: None,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        let state = Reducer::reduce(self.store.store()).map_err(|error| SubagentStartError {
            identity: Some(identity.clone()),
            error: error.to_string(),
        })?;
        let parent_run_id = state
            .lane("main")
            .and_then(|lane| lane.open_operation.clone());
        let parent_attempt = state.lane("main").map(|lane| lane.attempts);
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::SubagentLifecycle {
                id: format!("subagent-started-{}-{seq}", identity.run_id),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: parent_run_id,
                attempt: parent_attempt,
                child_run_id: TraceString::new(identity.run_id.clone()).map_err(|error| {
                    SubagentStartError {
                        identity: Some(identity.clone()),
                        error,
                    }
                })?,
                parent_tool_call_id: None,
                task_index: None,
                agent_id: TraceString::new(lane_hint).map_err(|error| SubagentStartError {
                    identity: Some(identity.clone()),
                    error,
                })?,
                subagent_lane: TraceString::new(identity.lane_name.clone()).map_err(|error| {
                    SubagentStartError {
                        identity: Some(identity.clone()),
                        error,
                    }
                })?,
                phase: SubagentLifecyclePhase::Started,
                result_entry_id: None,
                error: None,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        let identity = SubagentLaneIdentity {
            started_seq: self
                .store
                .records()
                .iter()
                .find_map(|record| match record {
                    HarnessRecord::OperationStarted { id, seq, .. } if id == &identity.run_id => {
                        Some(*seq)
                    }
                    _ => None,
                })
                .unwrap_or(0),
            ..identity
        };
        let accepted =
            self.accepted_subagent_run(&identity)
                .map_err(|error| SubagentStartError {
                    identity: Some(identity.clone()),
                    error,
                })?;
        Ok(StartedSubagentLane { identity, accepted })
    }

    pub fn accepted_subagent_run(
        &self,
        identity: &SubagentLaneIdentity,
    ) -> Result<AcceptedRun, String> {
        let accepted = AcceptedRun {
            session_id: self.store.session_id().to_owned(),
            run_id: identity.run_id.clone(),
            lane: identity.lane_name.clone(),
            prompt_entry_id: format!("entry-{}-user", identity.run_id),
            assistant_entry_id: format!("entry-{}-assistant-1", identity.run_id),
            accepted_through_seq: self
                .store
                .entries()
                .iter()
                .map(|entry| entry.seq)
                .chain(self.store.records().iter().map(HarnessRecord::seq))
                .max()
                .unwrap_or(0),
        };
        self.store
            .validate_accepted_run(&accepted)
            .map_err(|error| error.to_string())?;
        Ok(accepted)
    }

    /// Start a follow-up operation on an already-settled subagent lane
    /// (`hub revive` parity with oh-my-pi's parked-agent revive).
    ///
    /// The lane keeps its history: the child syncs the lane context, so the
    /// revived run continues where the previous turn left off. Fails when
    /// the lane is missing or still has an open operation (use `hub send`).
    pub fn resume_subagent_lane(
        &mut self,
        lane: &str,
        prompt: &str,
    ) -> Result<(SubagentLaneIdentity, AcceptedRun), String> {
        if prompt.trim().is_empty() {
            return Err("revive prompt must be non-empty".into());
        }
        self.ensure_fresh()?;
        let state = Reducer::reduce(self.store.store()).map_err(|error| error.to_string())?;
        let lane_state = state
            .lane(lane)
            .ok_or_else(|| format!("unknown subagent lane: {lane}"))?;
        if lane_state.open_operation.is_some() {
            return Err(format!(
                "lane {lane} is still live; use `hub send` to steer it"
            ));
        }
        let run_id = self.unique_run_id("subagent-run")?;
        let source_leaf_id = lane_state.leaf_id.clone();
        if let Err(error) = self.store.start_operation_on_lane(
            lane,
            &run_id,
            source_leaf_id.clone(),
            OperationIntent::Run,
        ) {
            return Err(error.to_string());
        }
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        let prompt_message = AgentMessage::user(prompt.to_owned(), Vec::new());
        let assistant_entry_id = self
            .store
            .accept_prompt_on_lane(lane, &run_id, prompt_message)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        let started_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == &run_id => Some(*seq),
                _ => None,
            })
            .unwrap_or(0);
        let identity = SubagentLaneIdentity {
            lane_name: lane.to_owned(),
            run_id: run_id.clone(),
            source_leaf_id,
            started_seq,
        };
        let accepted = AcceptedRun {
            session_id: self.store.session_id().to_owned(),
            run_id,
            lane: lane.to_owned(),
            prompt_entry_id: format!("entry-{}-user", identity.run_id),
            assistant_entry_id,
            accepted_through_seq: self
                .store
                .entries()
                .iter()
                .map(|entry| entry.seq)
                .chain(self.store.records().iter().map(HarnessRecord::seq))
                .max()
                .unwrap_or(0),
        };
        self.store
            .validate_accepted_run(&accepted)
            .map_err(|error| error.to_string())?;
        Ok((identity, accepted))
    }

    pub fn append_subagent_context(
        &mut self,
        lane: &str,
        run_id: &str,
        message: String,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let prompt_entry_id = format!("entry-{run_id}-user");
        if !self
            .store
            .entries()
            .iter()
            .any(|entry| entry.id == prompt_entry_id && entry.lane == lane)
        {
            return Err(format!("Missing accepted subagent task for lane {lane}"));
        }
        self.store
            .append_entry_gated(HarnessEntry {
                id: format!("entry-{run_id}-context-1"),
                parent_id: Some(prompt_entry_id),
                lane: lane.into(),
                seq: harness_next_seq(self.store.store()),
                timestamp: timestamp(),
                message: AgentMessage::user(message, Vec::new()),
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub fn finish_subagent_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let is_open = Reducer::reduce(self.store.store()).ok().map(|state| {
            state
                .lanes
                .iter()
                .any(|l| l.open_operation.as_deref() == Some(run_id))
        }) == Some(true);
        if !is_open {
            return Ok(());
        }

        if outcome == OperationOutcome::Aborted {
            let mut any_provisioned = false;
            if let Ok(state) = Reducer::reduce(self.store.store()) {
                if let Some(l) = state
                    .lanes
                    .iter()
                    .find(|l| l.open_operation.as_deref() == Some(run_id))
                {
                    for tool in &l.tools {
                        if !tool.completed
                            && tool.run_id == run_id
                            && !self
                                .store
                                .entries()
                                .iter()
                                .any(|entry| entry.id == tool.result_entry_id)
                        {
                            self.append_message_to_lane(
                                &l.name,
                                run_id,
                                AgentMessage::Tool {
                                    tool_call_id: tool.tool_call_id.clone(),
                                    name: tool.tool_name.clone(),
                                    content: error
                                        .clone()
                                        .unwrap_or_else(|| "Tool execution cancelled.".into()),
                                    is_error: true,
                                    terminate: false,
                                    images: Vec::new(),
                                },
                            )?;
                            any_provisioned = true;
                        }
                    }
                }
            }
            if any_provisioned {
                self.refresh().map_err(|error| error.to_string())?;
            }
            // Best-effort abort request; reconcile errors are observed below
            // but must not skip the terminal lifecycle record.
            let _ = self.store.request_abort(run_id);
            let _ = self.store.drive_to_completion();
            let _ = self.refresh();
            if self.store.reconcile_abort_run(run_id).is_ok() {
                let _ = self.store.drive_to_completion();
            }
        }

        self.store
            .finish_operation(run_id, outcome.clone(), error.clone())
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;

        let phase = match outcome {
            OperationOutcome::Completed => SubagentLifecyclePhase::Completed,
            OperationOutcome::Failed => SubagentLifecyclePhase::Failed,
            OperationOutcome::Aborted | OperationOutcome::Declined => {
                SubagentLifecyclePhase::Cancelled
            }
        };
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::SubagentLifecycle {
                id: format!("subagent-finished-{run_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: None,
                attempt: None,
                child_run_id: TraceString::new(run_id.to_owned())?,
                parent_tool_call_id: None,
                task_index: None,
                agent_id: TraceString::new(lane.to_owned())?,
                subagent_lane: TraceString::new(lane.to_owned())?,
                phase,
                result_entry_id: None,
                error: error.map(TraceString::new).transpose()?,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }
}
