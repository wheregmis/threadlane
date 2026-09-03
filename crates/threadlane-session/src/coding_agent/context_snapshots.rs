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
pub(crate) const MAX_COMPACTED_CONTEXT_INDEX_CHARS: usize = 4_000;

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
            .map_err(|error| error.to_string())?
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

    fn record_load(
        &self,
        context_id: &str,
        source_lane: &str,
        current_digest: Option<TraceString>,
        outcome: ContextSnapshotLoadOutcome,
    ) {
        if let Err(error) = CodingSessionHarness::record_context_snapshot_load_to_path(
            &self.session_file,
            context_id,
            source_lane,
            current_digest,
            outcome,
        ) {
            log::warn!("failed to persist context snapshot load outcome: {error}");
        }
    }

    fn list(&self, path: Option<&str>) -> Result<String, String> {
        let snapshots = self.snapshots()?;
        let lines = snapshots
            .into_iter()
            .filter(|snapshot| path.is_none_or(|path| snapshot.path == path))
            .take(MAX_CONTEXT_LIST_RESULTS)
            .map(|snapshot| snapshot_header(&snapshot, snapshot.file_sha256.as_str()))
            .collect::<Vec<_>>();
        Ok(if lines.is_empty() {
            "No context snapshots.".into()
        } else {
            lines.join("\n")
        })
    }

    fn load(&self, context_id: &str) -> Result<String, String> {
        let snapshot = match self.snapshot(context_id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.record_load(
                    context_id,
                    "main",
                    None,
                    ContextSnapshotLoadOutcome::Corrupt,
                );
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
                );
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
                self.record_load(context_id, source_lane, current_digest, outcome);
                Err(error)
            }
        }
    }
}

fn snapshot_header(snapshot: &ContextSnapshot, digest: &str) -> String {
    format!(
        "[Context snapshot {} from {}:{}-{}; digest {}]",
        snapshot.context_id,
        snapshot.path,
        snapshot
            .start_line
            .map_or("?".into(), |line| line.to_string()),
        snapshot
            .end_line
            .map_or("?".into(), |line| line.to_string()),
        digest,
    )
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
                    "path": {"type": "string"}
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
        if arguments
            .keys()
            .any(|key| !matches!(key.as_str(), "action" | "context_id" | "path"))
        {
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
            "list" => match arguments.get("path") {
                None => self.list(None),
                Some(Value::String(path)) => self.list(Some(path)),
                Some(_) => Err("Invalid manage_context arguments: path must be a string".into()),
            },
            "load" => match arguments.get("context_id").and_then(Value::as_str) {
                Some(context_id) if !context_id.is_empty() => self.load(context_id),
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
    !["http:", "https:", "virtual:", "file:"]
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
    let snapshot = store
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
        })
        .ok_or_else(|| format!("Context snapshot missing: {context_id}"))?;
    let path = threadlane_tools::validate_path_in_workspace(&snapshot.path, work_dir)
        .map_err(|error| format!("Context snapshot corrupt: {error}"))?;
    let digest =
        file_sha256(&path).map_err(|error| format!("Context snapshot missing: {error}"))?;
    if digest != snapshot.file_sha256 {
        return Err(format!("Context snapshot stale: {}", snapshot.path));
    }
    let entry = store
        .entries()
        .iter()
        .find(|entry| entry.id == snapshot.source_entry_id)
        .ok_or_else(|| format!("Context snapshot corrupt: {}", snapshot.context_id))?;
    if entry.lane != snapshot.source_lane {
        return Err(format!("Context snapshot corrupt: {}", snapshot.context_id));
    }
    let AgentMessage::Tool {
        tool_call_id,
        name,
        content,
        is_error: false,
        ..
    } = &entry.message
    else {
        return Err(format!("Context snapshot corrupt: {}", snapshot.context_id));
    };
    if tool_call_id != &snapshot.source_tool_call_id || name != "read_file" {
        return Err(format!("Context snapshot corrupt: {}", snapshot.context_id));
    }
    Ok(ResolvedContextSnapshot {
        snapshot,
        content: content.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        is_local_path, ContextSnapshotLoadOutcome, ContextSnapshotToolExecutor, JsonlStore, Record,
        MAX_CONTEXT_LIST_RESULTS,
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
        let entry_id = harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "read-1".into(),
                name: "read_file".into(),
                content: "snapshot body".into(),
                is_error: false,
                terminate: false,
            })
            .unwrap();
        let context_id = harness
            .index_read_snapshot(&run_id, dir.path(), "read-1", &entry_id, 13)
            .unwrap()
            .unwrap();
        (dir, path, context_id)
    }

    #[test]
    fn unicode_local_paths_do_not_panic_during_scheme_detection() {
        assert!(is_local_path("ééé"));
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
        assert_eq!(
            loaded,
            format!(
                "[Context snapshot {context_id} from README.md:1-1; digest {}]\nsnapshot body",
                super::file_sha256(&dir.path().join("README.md"))
                    .unwrap()
                    .as_str(),
            )
        );

        std::fs::write(dir.path().join("README.md"), "changed").unwrap();
        let stale = executor
            .execute_tool("manage_context", &load_args)
            .await
            .unwrap()
            .unwrap_err();
        assert!(stale.starts_with("Context snapshot stale:"));
        assert!(!stale.contains("snapshot body"));
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
    }
}
