use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use threadlane_runtime::harness::{
    ContextSnapshot, ContextSnapshotLoadOutcome, JsonlStore, Record, Reducer, TraceString,
};
use threadlane_runtime::{AgentMessage, AgentToolDefinition, ToolExecutor};

use super::durable::sha256_hex;
use super::harness::CodingSessionHarness;

#[allow(dead_code)]
pub(crate) const MAX_CONTEXT_LIST_RESULTS: usize = 20;
#[allow(dead_code)]
pub(crate) const MAX_SUBAGENT_CONTEXT_REFS: usize = 16;
#[allow(dead_code)]
pub(crate) const MAX_SUBAGENT_CONTEXT_CHARS: usize = 32_000;

#[allow(dead_code)]
pub(crate) struct ResolvedContextSnapshot {
    pub(crate) snapshot: ContextSnapshot,
    pub(crate) content: String,
}

pub(crate) struct ContextSnapshotToolExecutor {
    session_file: PathBuf,
    work_dir: PathBuf,
}

impl ContextSnapshotToolExecutor {
    pub(crate) fn new(session_file: PathBuf, work_dir: PathBuf) -> Self {
        Self {
            session_file,
            work_dir,
        }
    }

    fn snapshots(&self) -> Result<Vec<ContextSnapshot>, String> {
        let store = JsonlStore::open(&self.session_file)
            .map_err(|error| format!("Context snapshot corrupt: {error}"))?;
        let mut snapshots = Reducer::reduce(&store)
            .map_err(|error| format!("Context snapshot corrupt: {error}"))?
            .lane("main")
            .map(|lane| lane.context_snapshots.clone())
            .unwrap_or_default();
        snapshots.reverse();
        Ok(snapshots)
    }

    fn snapshot(&self, context_id: &str) -> Result<Option<ContextSnapshot>, String> {
        let store = JsonlStore::open(&self.session_file)
            .map_err(|error| format!("Context snapshot corrupt: {error}"))?;
        Ok(store
            .records()
            .iter()
            .rev()
            .find_map(|record| match record {
                Record::ContextSnapshotIndexed { snapshot, .. }
                    if snapshot.context_id == context_id =>
                {
                    Some(snapshot.clone())
                }
                _ => None,
            }))
    }

    async fn record_load(
        &self,
        context_id: &str,
        source_lane: &str,
        current_digest: Option<TraceString>,
        outcome: ContextSnapshotLoadOutcome,
    ) -> Result<(), String> {
        CodingSessionHarness::record_context_snapshot_load_to_path(
            &self.session_file,
            context_id,
            source_lane,
            current_digest,
            outcome,
        )
        .await
        .map_err(|error| format!("Context snapshot telemetry persistence failed: {error}"))
    }

    fn list(&self, path: Option<&str>, before_context_id: Option<&str>) -> Result<String, String> {
        let mut snapshots = self
            .snapshots()?
            .into_iter()
            .filter(|snapshot| path.is_none_or(|path| snapshot.path == path))
            .collect::<Vec<_>>();
        if let Some(before_context_id) = before_context_id {
            let index = snapshots
                .iter()
                .position(|snapshot| snapshot.context_id == before_context_id)
                .ok_or_else(|| format!("Context snapshot cursor missing: {before_context_id}"))?;
            snapshots.drain(..=index);
        }
        let lines = snapshots
            .into_iter()
            .take(MAX_CONTEXT_LIST_RESULTS)
            .map(|snapshot| snapshot_header(&snapshot, snapshot.file_sha256.as_str()))
            .collect::<Vec<_>>();
        Ok(if lines.is_empty() {
            "No context snapshots.".into()
        } else {
            lines.join("\n")
        })
    }

    async fn load(&self, context_id: &str) -> Result<String, String> {
        let snapshot = match self.snapshot(context_id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Err(telemetry_error) = self
                    .record_load(
                        context_id,
                        "main",
                        None,
                        ContextSnapshotLoadOutcome::Corrupt,
                    )
                    .await
                {
                    return Err(format!("{error}; {telemetry_error}"));
                }
                return Err(error);
            }
        };
        let source_lane = snapshot
            .as_ref()
            .map_or("main", |snapshot| snapshot.source_lane.as_str());
        let result = resolve_context_snapshot(&self.session_file, &self.work_dir, context_id);
        match result {
            Ok(resolved) => {
                self.record_load(
                    context_id,
                    &resolved.snapshot.source_lane,
                    Some(resolved.snapshot.file_sha256.clone()),
                    ContextSnapshotLoadOutcome::Loaded,
                )
                .await?;
                Ok(format!(
                    "{}\n{}",
                    snapshot_header(&resolved.snapshot, resolved.snapshot.file_sha256.as_str()),
                    resolved.content
                ))
            }
            Err(error) => {
                let outcome = if error.starts_with("Context snapshot stale:") {
                    ContextSnapshotLoadOutcome::Stale
                } else if error.starts_with("Context snapshot corrupt:") {
                    ContextSnapshotLoadOutcome::Corrupt
                } else {
                    ContextSnapshotLoadOutcome::Missing
                };
                let current_digest = snapshot.as_ref().and_then(|snapshot| {
                    threadlane_tools::validate_path_in_workspace(&snapshot.path, &self.work_dir)
                        .ok()
                        .and_then(|path| file_sha256(&path).ok())
                });
                match self
                    .record_load(context_id, source_lane, current_digest, outcome)
                    .await
                {
                    Ok(()) => Err(error),
                    Err(telemetry_error) => Err(format!("{error}; {telemetry_error}")),
                }
            }
        }
    }
}

fn snapshot_header(snapshot: &ContextSnapshot, digest: &str) -> String {
    format!(
        "[Context snapshot {} from {}; digest {}]",
        snapshot.context_id,
        snapshot_location(snapshot),
        digest,
    )
}

pub(crate) fn snapshot_location(snapshot: &ContextSnapshot) -> String {
    match (snapshot.start_line, snapshot.end_line) {
        (None, None) => snapshot.path.clone(),
        (start, end) => format!(
            "{}:{}-{}",
            snapshot.path,
            start.map_or_else(String::new, |line| line.to_string()),
            end.map_or_else(String::new, |line| line.to_string())
        ),
    }
}

pub(crate) fn compacted_context_snapshot_index_for_sources(
    snapshots: &[ContextSnapshot],
    prioritized_source_entry_ids: &[String],
) -> Vec<serde_json::Value> {
    let prioritized = prioritized_source_entry_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut index = Vec::new();
    for snapshot in snapshots
        .iter()
        .rev()
        .filter(|snapshot| prioritized.contains(snapshot.source_entry_id.as_str()))
        .chain(
            snapshots
                .iter()
                .rev()
                .filter(|snapshot| !prioritized.contains(snapshot.source_entry_id.as_str())),
        )
        .take(threadlane_runtime::compaction::MAX_CONTEXT_SNAPSHOT_INDEX_ENTRIES)
    {
        index.push(serde_json::json!({
            "context_id": snapshot.context_id,
            "path": snapshot.path,
            "start_line": snapshot.start_line,
            "end_line": snapshot.end_line,
            "file_sha256": snapshot.file_sha256.as_str(),
        }));
        if serde_json::to_string(&index).map_or(usize::MAX, |value| value.chars().count())
            > threadlane_runtime::compaction::MAX_CONTEXT_SNAPSHOT_INDEX_CHARS
        {
            index.pop();
            break;
        }
    }
    index
}

#[async_trait]
impl ToolExecutor for ContextSnapshotToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.manage_context"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        vec![AgentToolDefinition::new(
            "manage_context",
            "List or load durable read_file context snapshots from this session.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "load"]},
                    "context_id": {"type": "string"},
                    "path": {"type": "string"},
                    "before_context_id": {"type": "string", "description": "For list pagination, return snapshots older than this context ID."}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        )]
        .into()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        if name != "manage_context" {
            return None;
        }
        let arguments: Value = match serde_json::from_str(args) {
            Ok(arguments) => arguments,
            Err(error) => return Some(Err(format!("Invalid manage_context arguments: {error}"))),
        };
        let Some(arguments) = arguments.as_object() else {
            return Some(Err(
                "Invalid manage_context arguments: expected an object".into()
            ));
        };
        if arguments.keys().any(|key| {
            !matches!(
                key.as_str(),
                "action" | "context_id" | "path" | "before_context_id"
            )
        }) {
            return Some(Err(
                "Invalid manage_context arguments: unexpected property".into()
            ));
        }
        let Some(action) = arguments.get("action").and_then(Value::as_str) else {
            return Some(Err(
                "Invalid manage_context arguments: action is required".into()
            ));
        };
        Some(match action {
            "list" => {
                let path = match arguments.get("path") {
                    None => None,
                    Some(Value::String(path)) => Some(path.as_str()),
                    Some(_) => {
                        return Some(Err(
                            "Invalid manage_context arguments: path must be a string".into(),
                        ))
                    }
                };
                let before_context_id =
                    match arguments.get("before_context_id") {
                        None => None,
                        Some(Value::String(context_id)) => Some(context_id.as_str()),
                        Some(_) => return Some(Err(
                            "Invalid manage_context arguments: before_context_id must be a string"
                                .into(),
                        )),
                    };
                self.list(path, before_context_id)
            }
            "load" => match arguments.get("context_id").and_then(Value::as_str) {
                Some(context_id) if !context_id.is_empty() => self.load(context_id).await,
                _ => Err("Invalid manage_context arguments: context_id is required".into()),
            },
            _ => Err("Invalid manage_context arguments: action must be list or load".into()),
        })
    }
}

pub(crate) fn read_file_request(arguments: &Value) -> Option<(&str, Option<usize>, Option<usize>)> {
    let path = arguments.get("path")?.as_str()?;
    Some((
        path,
        arguments
            .get("start_line")
            .and_then(Value::as_u64)
            .map(|line| line as usize),
        arguments
            .get("end_line")
            .and_then(Value::as_u64)
            .map(|line| line as usize),
    ))
}

pub(crate) fn is_local_path(path: &str) -> bool {
    ![
        "http:", "https:", "virtual:", "file:", "skill:", "agent:", "pr:", "mr:", "issue:",
    ]
    .iter()
    .any(|prefix| {
        path.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    })
}

pub(crate) fn file_sha256(path: &Path) -> Result<TraceString, String> {
    TraceString::new(sha256_hex(
        &fs::read(path).map_err(|error| error.to_string())?,
    ))
    .map_err(|error| error.to_string())
}

pub(crate) fn resolve_context_snapshot(
    session_file: &Path,
    work_dir: &Path,
    context_id: &str,
) -> Result<ResolvedContextSnapshot, String> {
    let store = JsonlStore::open(session_file)
        .map_err(|error| format!("Context snapshot corrupt: {error}"))?;
    let raw_snapshot_exists = store
        .records()
        .iter()
        .any(|record| matches!(record, Record::ContextSnapshotIndexed { snapshot, .. } if snapshot.context_id == context_id));
    let snapshot = Reducer::reduce(&store)
        .map_err(|error| format!("Context snapshot corrupt: {error}"))?
        .lane("main")
        .and_then(|lane| {
            lane.context_snapshots
                .iter()
                .find(|snapshot| snapshot.context_id == context_id)
        })
        .cloned()
        .ok_or_else(|| {
            if raw_snapshot_exists {
                format!("Context snapshot corrupt: {context_id}")
            } else {
                format!("Context snapshot missing: {context_id}")
            }
        })?;
    let corrupt = || format!("Context snapshot corrupt: {}", snapshot.context_id);
    if snapshot.context_id != format!("ctx-{}", snapshot.source_entry_id) {
        return Err(corrupt());
    }
    let source_intent_matches = store.records().iter().any(|record| {
        matches!(
            record,
            Record::ToolStarted {
                lane,
                run_id,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                ..
            } if lane == &snapshot.source_lane
                && run_id == &snapshot.source_run_id
                && tool_call_id == &snapshot.source_tool_call_id
                && tool_name == "read_file"
                && result_entry_id == &snapshot.source_entry_id
                && read_file_request(effective_args).is_some_and(|(_, start, end)| {
                    start == snapshot.start_line && end == snapshot.end_line
                })
        )
    });
    if !source_intent_matches {
        return Err(corrupt());
    }
    let entry = store
        .entries()
        .iter()
        .find(|entry| entry.id == snapshot.source_entry_id)
        .ok_or_else(&corrupt)?;
    if entry.lane != snapshot.source_lane {
        return Err(corrupt());
    }
    let AgentMessage::Tool {
        tool_call_id,
        name,
        content,
        is_error: false,
        ..
    } = &entry.message
    else {
        return Err(corrupt());
    };
    if tool_call_id != &snapshot.source_tool_call_id
        || name != "read_file"
        || threadlane_tools::read_file_snapshot_digest(content)
            != Some(snapshot.file_sha256.as_str())
        || threadlane_tools::read_file_snapshot_path(content).as_deref()
            != Some(snapshot.path.as_str())
    {
        return Err(corrupt());
    }
    let path = threadlane_tools::validate_path_in_workspace(&snapshot.path, work_dir)
        .map_err(|error| format!("Context snapshot corrupt: {error}"))?;
    let location = snapshot_location(&snapshot);
    let digest = file_sha256(&path).map_err(|error| {
        format!(
            "Context snapshot missing: {location}; call read_file for current content ({error})"
        )
    })?;
    if digest != snapshot.file_sha256 {
        return Err(format!(
            "Context snapshot stale: {location}; call read_file for current content"
        ));
    }
    Ok(ResolvedContextSnapshot {
        snapshot,
        content: content.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        compacted_context_snapshot_index_for_sources, is_local_path, ContextSnapshotLoadOutcome,
        ContextSnapshotToolExecutor, JsonlStore, Record, Reducer, MAX_CONTEXT_LIST_RESULTS,
    };
    use crate::coding_agent::harness::CodingSessionHarness;
    use threadlane_runtime::harness::SessionStore;
    use threadlane_runtime::{AgentMessage, ToolExecutor};

    async fn snapshot_session() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(dir.path().join("README.md"), "snapshot body").unwrap();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("snapshot").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "read-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{\"path\":\"README.md\",\"start_line\":1,\"end_line\":1}"#
                            .into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "read-1",
                "read_file",
                serde_json::json!({"path": "README.md", "start_line": 1, "end_line": 1}),
            )
            .await
            .unwrap();
        let read_output = threadlane_tools::try_execute_tool_in_workspace(
            "read_file",
            r#"{"path":"README.md","start_line":1,"end_line":1}"#,
            dir.path(),
        )
        .unwrap();
        let output_chars = read_output.chars().count();
        let entry_id = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "read-1".into(),
                name: "read_file".into(),
                content: read_output,
                is_error: false,
                terminate: false,
            })
            .unwrap();
        let context_id = harness
            .index_read_snapshot(&run_id, dir.path(), "read-1", &entry_id, output_chars)
            .unwrap()
            .unwrap();
        (dir, path, context_id)
    }

    fn rewrite_snapshot(
        session_file: &std::path::Path,
        context_id: &str,
        rewrite: impl FnOnce(&mut serde_json::Value),
    ) {
        let mut rewrite = Some(rewrite);
        let session = std::fs::read_to_string(session_file).unwrap();
        let lines = session
            .lines()
            .map(|line| {
                let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
                if value["ContextSnapshotIndexed"]["snapshot"]["context_id"] == context_id {
                    rewrite.take().unwrap()(&mut value["ContextSnapshotIndexed"]["snapshot"]);
                }
                serde_json::to_string(&value).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rewrite.is_none(), "snapshot record was not found");
        std::fs::write(session_file, format!("{lines}\n")).unwrap();
    }

    #[tokio::test]
    async fn compacted_context_snapshot_index_keeps_metadata_not_body() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let snapshot = ContextSnapshotToolExecutor::new(session_file, dir.path().into())
            .snapshots()
            .unwrap()
            .pop()
            .unwrap();

        let index = compacted_context_snapshot_index_for_sources(&[snapshot], &[]);

        let value = serde_json::to_value(&index).unwrap();
        let entries = value.as_array().expect("structured snapshot index");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["context_id"], context_id);
        assert_eq!(entries[0]["path"], "README.md");
        assert_eq!(entries[0]["start_line"], 1);
        assert_eq!(entries[0]["end_line"], 1);
        assert!(entries[0]["file_sha256"].is_string());
        assert!(!value.to_string().contains("snapshot body"));
    }

    #[tokio::test]
    async fn compacted_context_snapshot_index_payload_is_bounded() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let source = super::resolve_context_snapshot(&session_file, dir.path(), &context_id)
            .unwrap()
            .snapshot;
        let snapshots = (0..40)
            .map(|index| {
                let mut snapshot = source.clone();
                snapshot.context_id = format!("ctx-{index}");
                snapshot.path = format!("{}-{index}.rs", "long-path/".repeat(60));
                snapshot
            })
            .collect::<Vec<_>>();

        let index = compacted_context_snapshot_index_for_sources(&snapshots, &[]);
        let value = serde_json::to_value(&index).unwrap();
        assert!(value.as_array().expect("structured snapshot index").len() <= 20);
        assert!(value.to_string().chars().count() <= 4_000);
    }

    #[tokio::test]
    async fn compacted_context_snapshot_index_prioritizes_sources_dropped_now() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let source = super::resolve_context_snapshot(&session_file, dir.path(), &context_id)
            .unwrap()
            .snapshot;
        let snapshots = (0..=MAX_CONTEXT_LIST_RESULTS)
            .map(|index| {
                let mut snapshot = source.clone();
                snapshot.context_id = format!("ctx-{index}");
                snapshot.source_entry_id = format!("source-{index}");
                snapshot
            })
            .collect::<Vec<_>>();

        let index = compacted_context_snapshot_index_for_sources(
            &snapshots,
            &[snapshots[0].source_entry_id.clone()],
        );

        assert_eq!(index.len(), MAX_CONTEXT_LIST_RESULTS);
        assert!(index.iter().any(|item| item["context_id"] == "ctx-0"));
        assert!(!index.iter().any(|item| item["context_id"] == "ctx-1"));
    }

    #[tokio::test]
    async fn harness_compaction_index_prioritizes_the_source_leaving_context() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let mut harness = CodingSessionHarness::open(&session_file).unwrap();
        let run_id = harness.unique_run_id("snapshot-priority").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        for index in 0..=MAX_CONTEXT_LIST_RESULTS {
            let tool_call_id = format!("read-{index}");
            let source_entry_id = harness
                .append_message(AgentMessage::Tool {
                    tool_call_id: tool_call_id.clone(),
                    name: "read_file".into(),
                    content: format!("body-{index}"),
                    is_error: false,
                    terminate: false,
                })
                .unwrap();
            harness
                .store
                .append_record_gated(Record::ContextSnapshotIndexed {
                    id: format!("index-{index}"),
                    seq: harness.store.store().next_sequence(),
                    lane: "main".into(),
                    timestamp: index as u64,
                    run_id: run_id.clone(),
                    snapshot: threadlane_runtime::harness::ContextSnapshot {
                        context_id: format!("ctx-{source_entry_id}"),
                        source_lane: "main".into(),
                        source_run_id: run_id.clone(),
                        source_tool_call_id: tool_call_id,
                        source_entry_id,
                        path: "README.md".into(),
                        start_line: None,
                        end_line: None,
                        file_sha256: threadlane_runtime::harness::TraceString::new("a".repeat(64))
                            .unwrap(),
                        output_chars: 6,
                        captured_at: index as u64,
                    },
                })
                .unwrap();
            harness.store.drive_to_completion().unwrap();
        }

        let index = harness.context_snapshot_index_for_compaction(2).unwrap();

        assert_eq!(index.len(), MAX_CONTEXT_LIST_RESULTS);
        assert!(index
            .iter()
            .any(|item| item["context_id"] == "ctx-v2-tool-result-read-0"));
        assert!(!index
            .iter()
            .any(|item| item["context_id"] == "ctx-v2-tool-result-read-1"));
    }

    #[test]
    fn unicode_local_paths_do_not_panic_during_scheme_detection() {
        assert!(is_local_path("ééé"));
        for path in [
            "skill://rust",
            "agent://reviewer",
            "pr://1",
            "mr://2",
            "issue://3",
            "http://example.com/file",
            "https://example.com/file",
        ] {
            assert!(!is_local_path(path), "{path}");
        }
    }

    #[tokio::test]
    async fn snapshot_rendering_omits_an_absent_range() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let mut snapshot = super::resolve_context_snapshot(&session_file, dir.path(), &context_id)
            .unwrap()
            .snapshot;
        snapshot.start_line = None;
        snapshot.end_line = None;

        assert_eq!(
            super::snapshot_header(&snapshot, snapshot.file_sha256.as_str()),
            format!(
                "[Context snapshot {context_id} from README.md; digest {}]",
                snapshot.file_sha256.as_str()
            )
        );
        assert_eq!(
            compacted_context_snapshot_index_for_sources(&[snapshot], &[])[0]["path"],
            "README.md"
        );
    }

    #[tokio::test]
    async fn manage_context_lists_no_snapshots_for_a_legacy_session() {
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        std::fs::write(
            &session_file,
            serde_json::json!({
                "id": "legacy-1",
                "parent_id": null,
                "timestamp": 1,
                "message": AgentMessage::user("legacy", vec![]),
            })
            .to_string(),
        )
        .unwrap();

        let store = JsonlStore::open(&session_file).unwrap();
        assert!(Reducer::reduce(&store)
            .unwrap()
            .lane("main")
            .unwrap()
            .context_snapshots
            .is_empty());
        let executor = ContextSnapshotToolExecutor::new(session_file, dir.path().into());
        assert_eq!(
            executor
                .execute_tool("manage_context", r#"{"action":"list"}"#)
                .await
                .unwrap()
                .unwrap(),
            "No context snapshots."
        );
    }

    #[tokio::test]
    async fn manage_context_loads_only_unchanged_current_session_snapshots() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let executor = ContextSnapshotToolExecutor::new(session_file, dir.path().into());
        let load_args = serde_json::json!({"action": "load", "context_id": context_id}).to_string();

        let loaded = executor
            .execute_tool("manage_context", &load_args)
            .await
            .unwrap()
            .unwrap();
        assert!(loaded.starts_with(&format!(
            "[Context snapshot {context_id} from README.md:1-1; digest {}]",
            super::file_sha256(&dir.path().join("README.md"))
                .unwrap()
                .as_str(),
        )));
        assert!(loaded.contains("snapshot body"));

        std::fs::write(dir.path().join("README.md"), "changed").unwrap();
        let stale = executor
            .execute_tool("manage_context", &load_args)
            .await
            .unwrap()
            .unwrap_err();
        assert!(stale.starts_with("Context snapshot stale:"));
        assert!(stale.contains("README.md:1-1"), "{stale}");
        assert!(stale.contains("call read_file"), "{stale}");
        assert!(!stale.contains("snapshot body"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn parallel_context_loads_each_persist_a_unique_success_record() {
        const LOADS: usize = 32;
        let (dir, session_file, context_id) = snapshot_session().await;
        let executor = std::sync::Arc::new(ContextSnapshotToolExecutor::new(
            session_file.clone(),
            dir.path().into(),
        ));
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(LOADS));
        let mut tasks = Vec::new();
        for _ in 0..LOADS {
            let executor = executor.clone();
            let barrier = barrier.clone();
            let load_args =
                serde_json::json!({"action": "load", "context_id": context_id}).to_string();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                executor
                    .execute_tool("manage_context", &load_args)
                    .await
                    .unwrap()
                    .unwrap()
            }));
        }
        for task in tasks {
            assert!(task.await.unwrap().contains("snapshot body"));
        }

        let store = JsonlStore::open(&session_file).unwrap();
        let ids = store
            .records()
            .iter()
            .filter_map(|record| match record {
                Record::ContextSnapshotLoaded {
                    id,
                    outcome: ContextSnapshotLoadOutcome::Loaded,
                    ..
                } => Some(id),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), LOADS);
    }

    #[tokio::test]
    async fn manage_context_rejects_unknown_and_malformed_arguments() {
        let (dir, session_file, _) = snapshot_session().await;
        let executor = ContextSnapshotToolExecutor::new(session_file, dir.path().into());

        assert_eq!(
            executor
                .execute_tool(
                    "manage_context",
                    r#"{"action":"load","context_id":"missing"}"#
                )
                .await
                .unwrap()
                .unwrap_err(),
            "Context snapshot missing: missing"
        );
        assert!(executor
            .execute_tool("manage_context", "not-json")
            .await
            .unwrap()
            .unwrap_err()
            .starts_with("Invalid manage_context arguments:"));
    }

    #[tokio::test]
    async fn manage_context_records_missing_and_corrupt_load_outcomes_without_content() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let executor = ContextSnapshotToolExecutor::new(session_file.clone(), dir.path().into());
        let load_args = serde_json::json!({"action": "load", "context_id": context_id}).to_string();

        std::fs::remove_file(dir.path().join("README.md")).unwrap();
        let missing = executor
            .execute_tool("manage_context", &load_args)
            .await
            .unwrap()
            .unwrap_err();
        assert!(missing.starts_with("Context snapshot missing:"));
        assert!(!missing.contains("snapshot body"));
        assert!(JsonlStore::open(&session_file)
            .unwrap()
            .records()
            .iter()
            .any(|record| matches!(
                record,
                Record::ContextSnapshotLoaded {
                    outcome: ContextSnapshotLoadOutcome::Missing,
                    ..
                }
            )));

        std::fs::write(dir.path().join("README.md"), "snapshot body").unwrap();
        let session = std::fs::read_to_string(&session_file).unwrap();
        std::fs::write(
            &session_file,
            session
                .lines()
                .map(|line| {
                    if line.contains(r#""id":"v2-tool-result-read-1""#) {
                        line.replacen(r#""name":"read_file""#, r#""name":"write_file""#, 1)
                    } else {
                        line.into()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let corrupt = executor
            .execute_tool("manage_context", &load_args)
            .await
            .unwrap()
            .unwrap_err();
        assert!(corrupt.starts_with("Context snapshot corrupt:"));
        assert!(!corrupt.contains("snapshot body"));
        assert!(JsonlStore::open(&session_file)
            .unwrap()
            .records()
            .iter()
            .any(|record| matches!(
                record,
                Record::ContextSnapshotLoaded {
                    outcome: ContextSnapshotLoadOutcome::Corrupt,
                    ..
                }
            )));
    }

    #[tokio::test]
    async fn manage_context_rejects_snapshot_metadata_that_fails_reduction() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let executor = ContextSnapshotToolExecutor::new(session_file.clone(), dir.path().into());
        let session = std::fs::read_to_string(&session_file).unwrap();
        std::fs::write(
            &session_file,
            session.replace(r#""source_run_id":""#, r#""source_run_id":"forged-"#),
        )
        .unwrap();

        let result = executor
            .execute_tool(
                "manage_context",
                &serde_json::json!({"action": "load", "context_id": context_id}).to_string(),
            )
            .await
            .unwrap()
            .unwrap_err();
        assert!(result.starts_with("Context snapshot corrupt:"));
        assert!(!result.contains("snapshot body"));
    }

    #[tokio::test]
    async fn manage_context_rejects_a_context_id_relabelled_onto_another_source() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let forged_id = "ctx-forged";
        rewrite_snapshot(&session_file, &context_id, |snapshot| {
            snapshot["context_id"] = forged_id.into();
        });
        let executor = ContextSnapshotToolExecutor::new(session_file, dir.path().into());

        let result = executor
            .execute_tool(
                "manage_context",
                &serde_json::json!({"action": "load", "context_id": forged_id}).to_string(),
            )
            .await
            .unwrap()
            .unwrap_err();

        assert!(result.starts_with("Context snapshot corrupt:"), "{result}");
        assert!(!result.contains("snapshot body"));
    }

    #[tokio::test]
    async fn manage_context_rejects_snapshot_metadata_that_disagrees_with_source_output() {
        let (dir, session_file, context_id) = snapshot_session().await;
        std::fs::write(dir.path().join("other.rs"), "other body").unwrap();
        let other_digest = super::file_sha256(&dir.path().join("other.rs")).unwrap();
        rewrite_snapshot(&session_file, &context_id, |snapshot| {
            snapshot["path"] = "other.rs".into();
            snapshot["file_sha256"] = other_digest.as_str().into();
        });
        let executor = ContextSnapshotToolExecutor::new(session_file, dir.path().into());

        let result = executor
            .execute_tool(
                "manage_context",
                &serde_json::json!({"action": "load", "context_id": context_id}).to_string(),
            )
            .await
            .unwrap()
            .unwrap_err();

        assert!(result.starts_with("Context snapshot corrupt:"), "{result}");
        assert!(!result.contains("snapshot body"));
    }

    #[tokio::test]
    async fn manage_context_rejects_snapshot_range_that_disagrees_with_durable_intent() {
        let (dir, session_file, context_id) = snapshot_session().await;
        rewrite_snapshot(&session_file, &context_id, |snapshot| {
            snapshot["start_line"] = 2.into();
            snapshot["end_line"] = 2.into();
        });
        let executor = ContextSnapshotToolExecutor::new(session_file, dir.path().into());

        let result = executor
            .execute_tool(
                "manage_context",
                &serde_json::json!({"action": "load", "context_id": context_id}).to_string(),
            )
            .await
            .unwrap()
            .unwrap_err();

        assert!(result.starts_with("Context snapshot corrupt:"), "{result}");
        assert!(!result.contains("snapshot body"));
    }

    #[tokio::test]
    async fn manage_context_rejects_a_snapshot_with_a_missing_source_entry() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let executor = ContextSnapshotToolExecutor::new(session_file.clone(), dir.path().into());
        let session = std::fs::read_to_string(&session_file).unwrap();
        std::fs::write(
            &session_file,
            session.replace(
                r#""source_entry_id":"v2-tool-result-read-1""#,
                r#""source_entry_id":"missing-entry""#,
            ),
        )
        .unwrap();

        let result = executor
            .execute_tool(
                "manage_context",
                &serde_json::json!({"action": "load", "context_id": context_id}).to_string(),
            )
            .await
            .unwrap()
            .unwrap_err();
        assert!(result.starts_with("Context snapshot corrupt:"));
        assert!(!result.contains("snapshot body"));
    }

    #[tokio::test]
    async fn manage_context_lists_current_session_snapshots_with_exact_path_filter() {
        let (dir, session_file, _) = snapshot_session().await;
        let executor = ContextSnapshotToolExecutor::new(session_file, dir.path().into());

        let all = executor
            .execute_tool("manage_context", r#"{"action":"list"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(all.contains("README.md"));
        assert_eq!(all.lines().count(), 1.min(MAX_CONTEXT_LIST_RESULTS));
        assert_eq!(
            executor
                .execute_tool("manage_context", r#"{"action":"list","path":"other.md"}"#)
                .await
                .unwrap()
                .unwrap(),
            "No context snapshots."
        );
    }

    #[tokio::test]
    async fn manage_context_lists_the_twenty_newest_snapshots() {
        let (dir, session_file, context_id) = snapshot_session().await;
        let mut store = JsonlStore::open(&session_file).unwrap();
        let source = super::resolve_context_snapshot(&session_file, dir.path(), &context_id)
            .unwrap()
            .snapshot;
        for index in 0..MAX_CONTEXT_LIST_RESULTS {
            let mut snapshot = source.clone();
            snapshot.context_id = format!("ctx-extra-{index}");
            store
                .append_record(Record::ContextSnapshotIndexed {
                    id: format!("snapshot-extra-{index}"),
                    seq: store.next_sequence(),
                    lane: "main".into(),
                    timestamp: index as u64,
                    run_id: snapshot.source_run_id.clone(),
                    snapshot,
                })
                .unwrap();
        }
        let executor = ContextSnapshotToolExecutor::new(session_file, dir.path().into());
        let listed = executor
            .execute_tool("manage_context", r#"{"action":"list","path":"README.md"}"#)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(listed.lines().count(), MAX_CONTEXT_LIST_RESULTS);
        assert!(listed.starts_with("[Context snapshot ctx-extra-19"));
        assert!(listed.contains("ctx-extra-0"));
        assert!(!listed.contains(&context_id));

        let older = executor
            .execute_tool(
                "manage_context",
                r#"{"action":"list","path":"README.md","before_context_id":"ctx-extra-0"}"#,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(older.lines().count(), 1);
        assert!(older.contains(&context_id));
    }
}
