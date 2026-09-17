use super::*;

impl CodingSessionHarness {
    pub fn index_read_snapshot(
        &mut self,
        run_id: &str,
        work_dir: &Path,
        tool_call_id: &str,
        source_entry_id: &str,
        output_chars: usize,
    ) -> Result<Option<String>, String> {
        self.ensure_fresh()?;
        let Some((effective_args, result_entry_id)) =
            self.store.records().iter().find_map(|record| match record {
                HarnessRecord::ToolStarted {
                    run_id: record_run_id,
                    tool_call_id: record_call_id,
                    tool_name,
                    effective_args,
                    result_entry_id,
                    ..
                } if record_run_id == run_id
                    && record_call_id == tool_call_id
                    && tool_name == "read_file" =>
                {
                    Some((effective_args, result_entry_id))
                }
                _ => None,
            })
        else {
            return Ok(None);
        };
        if result_entry_id != source_entry_id {
            return Ok(None);
        }
        let Some((requested_path, start_line, end_line)) = read_file_request(effective_args) else {
            return Ok(None);
        };
        if !is_local_path(requested_path) {
            return Ok(None);
        }
        let Some(entry) = self
            .store
            .entries()
            .iter()
            .find(|entry| entry.id == source_entry_id)
        else {
            return Ok(None);
        };
        let (digest, path) = match &entry.message {
            AgentMessage::Tool {
                tool_call_id: entry_call_id,
                name,
                content,
                is_error: false,
                ..
            } if entry_call_id == tool_call_id && name == "read_file" => {
                let Some(digest) = threadlane_tools::read_file_snapshot_digest(content) else {
                    return Ok(None);
                };
                let Some(path) = threadlane_tools::read_file_snapshot_path(content) else {
                    return Ok(None);
                };
                (
                    TraceString::new(digest.to_owned()).map_err(|error| error.to_string())?,
                    path,
                )
            }
            _ => return Ok(None),
        };
        let canonical_path = threadlane_tools::validate_path_in_workspace(&path, work_dir)?;
        let canonical_work_dir = work_dir.canonicalize().map_err(|error| error.to_string())?;
        let relative_path = canonical_path
            .strip_prefix(&canonical_work_dir)
            .map_err(|_| {
                format!(
                    "read path '{}' is outside workspace",
                    canonical_path.display()
                )
            })?
            .to_string_lossy()
            .into_owned();
        let context_id = format!("ctx-{source_entry_id}");
        if self.context_snapshots("main").iter().any(|snapshot| {
            snapshot.context_id == context_id
                && snapshot.source_run_id == run_id
                && snapshot.source_tool_call_id == tool_call_id
                && snapshot.source_entry_id == source_entry_id
        }) {
            return Ok(Some(context_id));
        }
        let snapshot = threadlane_runtime::harness::ContextSnapshot {
            context_id: context_id.clone(),
            source_lane: "main".into(),
            source_run_id: run_id.into(),
            source_tool_call_id: tool_call_id.into(),
            source_entry_id: source_entry_id.into(),
            path: relative_path,
            start_line,
            end_line,
            file_sha256: digest,
            output_chars,
            captured_at: timestamp(),
        };
        self.store
            .append_record_gated(HarnessRecord::ContextSnapshotIndexed {
                id: format!("context-snapshot-{context_id}"),
                seq: self.next_seq(),
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                snapshot,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(Some(context_id))
    }

    pub fn context_snapshots(
        &self,
        lane: &str,
    ) -> Vec<threadlane_runtime::harness::ContextSnapshot> {
        Reducer::reduce(self.store.store())
            .ok()
            .and_then(|state| state.lane(lane).map(|lane| lane.context_snapshots.clone()))
            .unwrap_or_default()
    }

    pub async fn record_context_snapshot_load_to_path(
        path: &Path,
        context_id: &str,
        source_lane: &str,
        current_digest: Option<TraceString>,
        outcome: ContextSnapshotLoadOutcome,
    ) -> Result<(), String> {
        let path = path.to_path_buf();
        let context_id = context_id.to_owned();
        let source_lane = source_lane.to_owned();
        tokio::task::spawn_blocking(move || {
            Self::with_path(&path, |journal| {
                journal.ensure_fresh()?;
                let run_id = Reducer::reduce(journal.store.store())
                    .ok()
                    .and_then(|state| {
                        state
                            .lane("main")
                            .and_then(|lane| lane.open_operation.clone())
                    })
                    .unwrap_or_else(|| "context-load".into());
                let seq = journal.next_seq();
                let record_id = format!(
                    "context-snapshot-load-{}-{}-{}",
                    std::process::id(),
                    timestamp(),
                    NEXT_CONTEXT_SNAPSHOT_LOAD_ID.fetch_add(1, Ordering::Relaxed),
                );
                journal
                    .store
                    .append_record_gated(HarnessRecord::ContextSnapshotLoaded {
                        id: record_id,
                        seq,
                        lane: "main".into(),
                        timestamp: timestamp(),
                        run_id,
                        context_id,
                        source_lane,
                        current_digest,
                        outcome,
                    })
                    .map_err(|error| error.to_string())?;
                journal
                    .store
                    .drive_to_completion()
                    .map_err(|error| error.to_string())
            })
        })
        .await
        .map_err(|error| error.to_string())?
    }
}
