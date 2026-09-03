use std::fs;
use std::path::Path;

use serde_json::Value;
use threadlane_runtime::harness::{ContextSnapshot, JsonlStore, Record, SessionStore, TraceString};
use threadlane_runtime::AgentMessage;

use super::durable::sha256_hex;

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
    !path.contains("://")
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
    let store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
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
