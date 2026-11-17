use super::*;

const KEEP_RECENT_READS: usize = 3;

/// Prefix of the reference text a duplicate read carries; the call id that
/// follows it names the earlier result whose body must stay inline for the
/// pointer to resolve.
pub(super) const UNCHANGED_READ_REFERENCE_PREFIX: &str =
    "[Unchanged read; full content remains in earlier tool result ";

/// Request-only reduction. The first visible copy of a recent read stays
/// inline; duplicate results point to it. Older reads are evictable only while
/// their snapshot is fresh and the request exposes manage_context.
fn reduce_read_context_with_digests(
    messages: &[AgentMessage],
    snapshots: &[threadlane_runtime::harness::ContextSnapshot],
    current_digests: &HashMap<String, String>,
    can_load: bool,
) -> Vec<AgentMessage> {
    let by_call: HashMap<_, _> = snapshots
        .iter()
        .map(|snapshot| (snapshot.source_tool_call_id.as_str(), snapshot))
        .collect();
    let snapshot_for = |message: &AgentMessage| match message {
        AgentMessage::Tool {
            name,
            tool_call_id,
            content,
            is_error: false,
            images,
            ..
        } if name == "read_file" && images.is_empty() => by_call
            .get(tool_call_id.as_str())
            .copied()
            .filter(|snapshot| {
                threadlane_tools::read_file_snapshot_digest(content)
                    == Some(snapshot.file_sha256.as_str())
                    && threadlane_tools::read_file_snapshot_path(content).as_deref()
                        == Some(snapshot.path.as_str())
            }),
        _ => None,
    };
    fn key(
        snapshot: &threadlane_runtime::harness::ContextSnapshot,
    ) -> (&str, Option<usize>, Option<usize>, &str) {
        (
            snapshot.path.as_str(),
            snapshot.start_line,
            snapshot.end_line,
            snapshot.file_sha256.as_str(),
        )
    }
    let mut recent = std::collections::HashSet::new();
    for snapshot in messages.iter().rev().filter_map(snapshot_for) {
        recent.insert(key(snapshot));
        if recent.len() == KEEP_RECENT_READS {
            break;
        }
    }
    let mut visible = HashMap::new();
    messages.iter().map(|message| {
        let Some(snapshot) = snapshot_for(message) else { return message.clone(); };
        let AgentMessage::Tool { tool_call_id, name, content, is_error, terminate, images } = message else { unreachable!() };
        let snapshot_key = key(snapshot);
        let replacement = if let Some(first_call) = visible.get(&snapshot_key) {
            Some(format!("{UNCHANGED_READ_REFERENCE_PREFIX}{first_call}. Do not repeat this read without changed arguments or file contents.]"))
        } else if can_load && !recent.contains(&snapshot_key) {
            let digest = current_digests.get(&snapshot.path);
            if digest.map(String::as_str) == Some(snapshot.file_sha256.as_str()) {
                Some(format!("[Earlier read of {} stored as context snapshot {}. Use manage_context(action=load, context_id=\"{}\") if needed.]",
                    crate::context_snapshots::snapshot_location(snapshot), snapshot.context_id, snapshot.context_id))
            } else { None }
        } else { None };
        // Small bodies cost less than a reference. Only refer to a copy that
        // is actually inline in this request, never to an evicted result.
        if let Some(replacement) = replacement.filter(|text| text.len() < content.len()) {
            AgentMessage::Tool {
                tool_call_id: tool_call_id.clone(),
                name: name.clone(),
                content: replacement,
                is_error: *is_error,
                terminate: *terminate,
                images: images.clone(),
            }
        } else {
            visible.entry(snapshot_key)
                .or_insert(snapshot.source_tool_call_id.as_str());
            message.clone()
        }
    }).collect()
}

fn can_load_read_snapshots(tool_schema_json: Option<&str>) -> bool {
    tool_schema_json
        .and_then(|schema| serde_json::from_str::<Value>(schema).ok())
        .and_then(|tools| {
            tools.as_array().map(|tools| {
                tools.iter().any(|tool| {
                    tool.pointer("/function/name").and_then(Value::as_str) == Some("manage_context")
                })
            })
        })
        .unwrap_or(false)
}

fn read_context_digests(paths: Vec<String>, work_dir: Option<PathBuf>) -> HashMap<String, String> {
    // ponytail: hash each candidate once per attempt; use a watcher-invalidated
    // digest cache if repeated I/O becomes costly.
    let Some(work_dir) = work_dir else {
        return HashMap::new();
    };
    paths
        .into_iter()
        .filter_map(|path| {
            let validated = threadlane_tools::validate_path_in_workspace(&path, &work_dir).ok()?;
            let bytes = fs::read(validated).ok()?;
            Some((path, crate::durable::sha256_hex(&bytes)))
        })
        .collect()
}

#[cfg(test)]
fn reduce_read_context(
    messages: &[AgentMessage],
    snapshots: &[threadlane_runtime::harness::ContextSnapshot],
    work_dir: Option<&Path>,
    can_load: bool,
) -> Vec<AgentMessage> {
    let digests = read_context_digests(
        snapshots
            .iter()
            .map(|snapshot| snapshot.path.clone())
            .collect(),
        work_dir.map(Path::to_path_buf),
    );
    reduce_read_context_with_digests(messages, snapshots, &digests, can_load)
}

#[cfg(test)]
mod reduction_tests {
    use super::{reduce_read_context, AgentMessage};
    use threadlane_runtime::harness::{ContextSnapshot, TraceString};

    /// Local measurement, never a timing assertion in CI. Run with
    /// `cargo nextest run -p threadlane-coding-agent --lib context_preparation_profile --run-ignored only --no-capture`.
    #[test]
    #[ignore]
    fn context_preparation_profile() {
        use std::{collections::HashMap, hint::black_box, time::Instant};
        use threadlane_compaction::{
            estimate_request_tokens, prune_historical_tool_outputs, CompactionParams,
        };

        let mut messages = Vec::new();
        let mut snapshots = Vec::new();
        let digest = "a".repeat(64);
        for index in 0..128 {
            let path = format!("file-{}.rs", index % 16);
            let content = format!(
                "[Threadlane read_file SHA-256: {digest}]\n[Threadlane read_file path: \"{path}\"]\n{}",
                "source 🦀\n".repeat(1_000)
            );
            let call = format!("call-{index}");
            snapshots.push(ContextSnapshot {
                context_id: format!("ctx-{index}"),
                source_lane: "main".into(),
                source_run_id: "run".into(),
                source_tool_call_id: call.clone(),
                source_entry_id: format!("result-{index}"),
                path,
                start_line: None,
                end_line: None,
                file_sha256: TraceString::new(&digest).unwrap(),
                output_chars: content.chars().count(),
                captured_at: 0,
            });
            messages.push(AgentMessage::Tool {
                tool_call_id: call,
                name: "read_file".into(),
                content,
                is_error: false,
                terminate: index == 127,
                images: Vec::new(),
            });
        }
        let digests = HashMap::new();
        let reduce = || {
            super::reduce_read_context_with_digests(&messages, &snapshots, &digests, false)
        };
        let prune = || prune_historical_tool_outputs(&messages, 3);
        let config = CompactionParams::default();
        let before = estimate_request_tokens(&messages, None, &config);
        for (name, operation) in [
            ("duplicate reads", &reduce as &dyn Fn() -> Vec<AgentMessage>),
            ("historical previews", &prune as &dyn Fn() -> Vec<AgentMessage>),
        ] {
            let result = operation();
            let after = estimate_request_tokens(&result, None, &config);
            let fingerprint = crate::durable::sha256_hex(&serde_json::to_vec(&result).unwrap());
            let mut samples = Vec::new();
            for _ in 0..7 {
                let start = Instant::now();
                for _ in 0..250 {
                    black_box(operation());
                }
                samples.push(start.elapsed().as_nanos() / 250);
            }
            samples.sort_unstable();
            eprintln!("{name}: median {} ns/prepare; estimated tokens {before} -> {after}; output SHA-256 {fingerprint}", samples[3]);
        }
    }

    #[test]
    fn eviction_requires_fresh_recoverable_reads_and_duplicates_require_visible_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let mut messages = Vec::new();
        let mut snapshots = Vec::new();
        for index in 0..4 {
            let path = format!("{index}.rs");
            std::fs::write(dir.path().join(&path), "body ".repeat(1000)).unwrap();
            let content = threadlane_tools::try_execute_tool_in_workspace(
                "read_file",
                &serde_json::json!({"path": path}).to_string(),
                dir.path(),
            )
            .unwrap();
            let call = format!("call-{index}");
            snapshots.push(ContextSnapshot {
                context_id: format!("ctx-result-{index}"),
                source_lane: "main".into(),
                source_run_id: "run".into(),
                source_tool_call_id: call.clone(),
                source_entry_id: format!("result-{index}"),
                path,
                start_line: None,
                end_line: None,
                file_sha256: TraceString::new(
                    threadlane_tools::read_file_snapshot_digest(&content).unwrap(),
                )
                .unwrap(),
                output_chars: content.chars().count(),
                captured_at: 0,
            });
            messages.push(AgentMessage::Tool {
                tool_call_id: call,
                name: "read_file".into(),
                content,
                is_error: false,
                terminate: false,
                images: vec![],
            });
        }
        let reduced = reduce_read_context(&messages, &snapshots, Some(dir.path()), true);
        assert!(
            matches!(&reduced[0], AgentMessage::Tool { content, .. } if content.contains("manage_context"))
        );
        assert_eq!(&reduced[1..], &messages[1..]);
        assert_eq!(
            reduce_read_context(&messages, &snapshots, Some(dir.path()), false),
            messages
        );
        assert_eq!(
            reduce_read_context(&messages, &snapshots, None, true),
            messages
        );
        std::fs::write(dir.path().join("0.rs"), "changed").unwrap();
        assert_eq!(
            reduce_read_context(&messages, &snapshots, Some(dir.path()), true),
            messages
        );
        std::fs::remove_file(dir.path().join("0.rs")).unwrap();
        assert_eq!(
            reduce_read_context(&messages, &snapshots, Some(dir.path()), true),
            messages
        );

        let mut duplicate = messages[0].clone();
        if let AgentMessage::Tool {
            tool_call_id, terminate, ..
        } = &mut duplicate {
            *tool_call_id = "duplicate".into();
            *terminate = true;
        }
        let mut snapshot = snapshots[0].clone();
        snapshot.source_tool_call_id = "duplicate".into();
        snapshots.push(snapshot);
        messages.push(duplicate);
        let reduced = reduce_read_context(&messages, &snapshots, Some(dir.path()), true);
        assert_eq!(reduced[0], messages[0]);
        assert!(
            matches!(reduced.last(), Some(AgentMessage::Tool { content, terminate: true, .. }) if content.contains("Unchanged read"))
        );
        // Different ranges and failures must retain their own contents.
        snapshots.last_mut().unwrap().start_line = Some(2);
        assert_eq!(
            reduce_read_context(&messages, &snapshots, Some(dir.path()), false),
            messages
        );
        snapshots.last_mut().unwrap().start_line = None;
        if let AgentMessage::Tool { is_error, .. } = messages.last_mut().unwrap() {
            *is_error = true;
        }
        assert_eq!(
            reduce_read_context(&messages, &snapshots, Some(dir.path()), false),
            messages
        );
    }
}

impl CodingSessionHarness {
    pub(super) fn provider_read_context(
        &self,
        messages: &[AgentMessage],
        tool_schema_json: Option<&str>,
        current_digests: &HashMap<String, String>,
    ) -> Vec<AgentMessage> {
        let can_load = can_load_read_snapshots(tool_schema_json);
        reduce_read_context_with_digests(
            messages,
            &self.context_snapshots("main"),
            current_digests,
            can_load,
        )
    }

    /// Copy the small freshness inputs while locked; workspace I/O happens later.
    pub(super) fn provider_read_digest_inputs(
        &mut self,
        run_id: &str,
    ) -> Result<(Vec<String>, Option<PathBuf>), String> {
        self.ensure_fresh()?;
        let work_dir = self
            .store
            .records()
            .iter()
            .rev()
            .find_map(|record| match record {
                HarnessRecord::RunContextCaptured {
                    run_id: captured_run,
                    work_dir,
                    ..
                } if captured_run == run_id => Some(PathBuf::from(work_dir.as_str())),
                _ => None,
            });
        let snapshots = self.context_snapshots("main");
        if snapshots.is_empty() {
            return Ok((Vec::new(), work_dir));
        }
        let messages = self.model_context("main")?.into_messages();
        let by_call: HashMap<_, _> = snapshots
            .iter()
            .map(|snapshot| (snapshot.source_tool_call_id.as_str(), snapshot))
            .collect();
        let mut recent = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for message in messages.iter().rev() {
            let AgentMessage::Tool {
                name,
                tool_call_id,
                is_error: false,
                images,
                ..
            } = message
            else {
                continue;
            };
            if name != "read_file" || !images.is_empty() {
                continue;
            }
            let Some(snapshot) = by_call.get(tool_call_id.as_str()) else {
                continue;
            };
            let key = (
                &snapshot.path,
                snapshot.start_line,
                snapshot.end_line,
                snapshot.file_sha256.as_str(),
            );
            if recent.contains(&key) {
                continue;
            }
            if recent.len() < KEEP_RECENT_READS {
                recent.insert(key);
            } else {
                paths.insert(snapshot.path.clone());
            }
        }
        Ok((paths.into_iter().collect(), work_dir))
    }

    pub(crate) async fn prepare_shared_provider_boundary(
        harness: Arc<tokio::sync::Mutex<Self>>,
        run_id: String,
        request: ProviderBoundaryRequest,
        config: AgentConfig,
    ) -> Result<ProviderBoundaryResult, String> {
        let (paths, work_dir) = {
            let mut locked = harness.lock().await;
            if can_load_read_snapshots(request.tool_schema_json.as_deref()) {
                locked.provider_read_digest_inputs(&run_id)?
            } else {
                (Vec::new(), None)
            }
        };
        // Freshness I/O must neither hold the recorder mutex nor block a Tokio worker.
        let digests = tokio::task::spawn_blocking(move || read_context_digests(paths, work_dir))
            .await
            .map_err(|error| format!("snapshot freshness task failed: {error}"))?;
        let mut locked = harness.lock().await;
        locked.prepare_provider_boundary(&run_id, request, &config, &digests)
    }

    pub(crate) fn index_read_snapshot(
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
        let Some(entry) = self.store.entry(source_entry_id) else {
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

    pub(crate) fn context_snapshots(
        &self,
        lane: &str,
    ) -> Vec<threadlane_runtime::harness::ContextSnapshot> {
        Reducer::reduce(self.store.store())
            .ok()
            .and_then(|state| state.lane(lane).map(|lane| lane.context_snapshots.clone()))
            .unwrap_or_default()
    }

    pub(crate) async fn record_context_snapshot_load_to_path(
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
