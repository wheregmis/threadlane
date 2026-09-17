use super::*;

impl CodingSessionHarness {
    // ── Replay & navigation ───────────────────────────────────────────

    /// Append a replayed tool entry to the store.
    pub fn append_replayed_tool_entry(
        &mut self,
        run_id: &str,
        assistant_entry_id: &str,
        spec: &ToolSpec,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        let state = Reducer::reduce(self.store.store()).map_err(|error| error.to_string())?;
        let lane = state
            .lanes
            .iter()
            .find(|lane| lane.open_operation.as_deref() == Some(run_id))
            .ok_or_else(|| format!("harness operation {run_id} is not open"))?;
        let parent_id = if spec.index == 0 {
            assistant_entry_id.to_string()
        } else {
            state
                .lanes
                .iter()
                .flat_map(|lane| lane.tools.iter())
                .find(|tool| {
                    tool.run_id == run_id
                        && tool.assistant_entry_id == assistant_entry_id
                        && tool.tool_index + 1 == spec.index
                })
                .filter(|tool| {
                    self.store
                        .entries()
                        .iter()
                        .any(|entry| entry.id == tool.result_entry_id)
                })
                .map(|tool| tool.result_entry_id.clone())
                .unwrap_or_else(|| assistant_entry_id.to_string())
        };
        let seq = self.next_seq();
        self.store
            .append_entry_gated(HarnessEntry {
                id: spec.result_entry_id.clone(),
                parent_id: Some(parent_id),
                lane: lane.name.clone(),
                seq,
                timestamp: timestamp(),
                message: AgentMessage::Tool {
                    tool_call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    terminate: result.terminates(),
                    images: result.images.clone(),
                },
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: result.terminates(),
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Claim safe tool replays for recovery.
    pub fn claim_safe_replays(
        &mut self,
        tools: &[HarnessRecord],
    ) -> Result<Vec<HarnessRecord>, String> {
        let records = self.store.records().to_vec();
        let entries = self.store.entries().to_vec();
        let mut claimed = Vec::new();
        for tool in tools {
            let HarnessRecord::ToolStarted {
                lane,
                run_id,
                assistant_entry_id,
                tool_index,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                replay: HarnessToolReplaySafety::Safe,
                ..
            } = tool
            else {
                continue;
            };
            let already_completed = records.iter().any(|record| {
                matches!(
                    record,
                    HarnessRecord::ToolFinished {
                        run_id: finished_run,
                        tool_call_id: finished_call,
                        result_entry_id: finished_result,
                        ..
                    } if finished_run == run_id
                        && finished_call == tool_call_id
                        && finished_result == result_entry_id
                )
            }) || entries.iter().any(|entry| entry.id == *result_entry_id);
            if already_completed {
                continue;
            }
            let seq = self.next_seq();
            self.store
                .append_record_gated(HarnessRecord::ToolStarted {
                    id: format!("replay-claim-{run_id}-{tool_call_id}-{seq}"),
                    seq,
                    lane: lane.clone(),
                    timestamp: timestamp(),
                    run_id: run_id.clone(),
                    assistant_entry_id: assistant_entry_id.clone(),
                    tool_index: *tool_index,
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    effective_args: effective_args.clone(),
                    result_entry_id: result_entry_id.clone(),
                    replay: HarnessToolReplaySafety::Never,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            claimed.push(tool.clone());
        }
        Ok(claimed)
    }

    /// Materialize a session branch path as harness entries.
    pub fn navigate_branch(
        &mut self,
        branch_ids: &[String],
    ) -> Result<Option<String>, String> {
        self.ensure_fresh()?;
        let mut harness_target_id = None;
        for legacy_id in branch_ids {
            let entry = self
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == *legacy_id)
                .cloned()
                .ok_or_else(|| format!("Entry ID not found in session: {legacy_id}"))?;
            if matches!(entry.message, AgentMessage::System { .. }) {
                continue;
            }
            if *legacy_id == branch_ids[branch_ids.len() - 1] {
                harness_target_id = Some(entry.id);
            }
        }
        Ok(harness_target_id)
    }

}
