use super::*;

impl CodingSessionHarness {
    /// Claim safe tool replays for recovery.
    pub(crate) fn claim_safe_replays(
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
}
