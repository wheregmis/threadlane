//! Tool execution dispatcher.
//!
//! Owns the executor registry, hook pipeline, and parallel/sequential dispatch logic.
//! Independently testable.

use crate::error::AgentError;
use threadlane_protocol::AgentEvent;
use crate::harness::{HookContext, HookRegistry};
use crate::tool_executor::builtin_tool_executor;
use crate::types::ToolExecutionMode;
use crate::utils::AbortOnDrop;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_protocol::RuntimeToolCall as ToolCall;
use threadlane_protocol::{
    AgentToolCall, AgentToolDefinition, AgentToolResult, ImageAttachment, RecoveredToolReply,
    ToolExecutionError, ToolExecutionIdentity, ToolExecutor, ToolOutput,
};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use futures::FutureExt;

/// Resolve the auxiliary CLI form through the same executor path in both modes.
fn dyn_tool_call(run_command_arguments: &str) -> Option<(String, String)> {
    let arguments: Value = serde_json::from_str(run_command_arguments).ok()?;
    let command = arguments.get("command")?.as_str()?.trim();
    let input = command.strip_prefix("dyn ")?.trim();
    let mut parts = input.split_whitespace();
    let tool_name = parts.next()?;
    let remaining = input[tool_name.len()..].trim();
    let remaining = threadlane_tools::dispatch::strip_matching_outer_quotes(remaining)
        .unwrap_or(remaining)
        .trim();
    if tool_name.starts_with('-') || remaining == "--help" || remaining == "-h" {
        return None;
    }
    let tool_arguments = if remaining.is_empty() {
        "{}".to_owned()
    } else if remaining.starts_with('{')
        && serde_json::from_str::<Value>(remaining)
            .ok()
            .is_some_and(|value| value.is_object())
    {
        remaining.to_owned()
    } else {
        return None;
    };
    Some((tool_name.to_owned(), tool_arguments))
}
fn resolve_tool_call(
    tc: &ToolCall,
    work_dir: Option<&Path>,
) -> (AgentToolCall, String, Option<String>) {
    let mut arguments =
        normalize_tool_arguments(&tc.function.name, &tc.function.arguments, work_dir);
    let mut agent_tool_call = AgentToolCall {
        id: tc.id.clone(),
        name: tc.function.name.clone(),
        arguments: arguments.clone(),
    };
    let mut intent_arguments = None;
    while agent_tool_call.name == "run_command" {
        let Some((name, args)) = dyn_tool_call(&arguments) else {
            break;
        };
        if intent_arguments.is_none() {
            intent_arguments = Some(std::mem::take(&mut arguments));
        }
        arguments = normalize_tool_arguments(&name, &args, work_dir);
        agent_tool_call.name = name;
        agent_tool_call.arguments = arguments.clone();
    }

    (agent_tool_call, arguments, intent_arguments)
}

/// Callback invoked after hooks to commit intent before execution.
pub type ToolIntentRecorder = crate::provider::ToolIntentRecorder;
/// Callback invoked after tool execution completes.
pub type ToolCompletionRecorder = crate::provider::ToolCompletionRecorder;

#[derive(Clone)]
struct ToolExecutorRoute {
    executor: Arc<dyn ToolExecutor>,
    tool_names: HashSet<String>,
}

struct ToolRunContext {
    hooks: HookRegistry,
    intent_recorder: Option<ToolIntentRecorder>,
    execution_trace_recorder: Option<crate::provider::ToolExecutionTraceRecorder>,
    event_tx: broadcast::Sender<AgentEvent>,
    tool_routes: Arc<[ToolExecutorRoute]>,
    allowed_tool_names: Option<Arc<HashSet<String>>>,
    work_dir: Option<PathBuf>,
    skip_before_hook: bool,
    session_id: String,
    repetition: RepetitionCacheHandle,
    /// Replay re-executes safe tools for verification and must observe live
    /// state, never cached results.
    skip_repetition_cache: bool,
}

/// Deduplicates identical tool calls within one turn: the 37×-identical-read
/// failure mode. A mutation revision fences validation and insertion while
/// filesystem probes run outside the shared lock.
/// Cloned dispatchers share one cache through the `Arc`.
#[derive(Clone, Default)]
struct RepetitionCacheHandle {
    inner: Arc<std::sync::Mutex<RepetitionCache>>,
}

#[derive(Default)]
struct RepetitionCache {
    revision: u64,
    entries: std::collections::HashMap<(String, String), Arc<CachedToolResult>>,
}

struct CachedToolResult {
    content: String,
    is_error: bool,
    images: Vec<ImageAttachment>,
    /// Canonicalized absolute path this entry depends on, if it reads one
    /// file (currently only `read_file`). Lets same-path writes invalidate
    /// precisely while unrelated writes keep their cache.
    path: Option<PathBuf>,
    /// File size + mtime at cache time, for external-mutation validation.
    fingerprint: Option<FileFingerprint>,
    /// Canonicalized workspace root plus a complete, bounded fingerprint, for
    /// workspace-wide reads (`grep_search`, `list_dir`, `get_repo_map`)
    /// whose inputs are the whole tree rather than one file.
    tree: Option<(PathBuf, TreeFingerprint)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    size: u64,
    mtime_secs: u64,
    mtime_nanos: u32,
}

fn fingerprint_file(path: &Path) -> Option<FileFingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(FileFingerprint {
        size: metadata.len(),
        mtime_secs: duration.as_secs(),
        mtime_nanos: duration.subsec_nanos(),
    })
}

/// Complete fingerprint of a workspace tree, bounded by
/// `TREE_FINGERPRINT_BUDGET` files and directories. Partial or unreadable scans
/// verify nothing and must not enable caching. Entries sort before hashing.
const TREE_FINGERPRINT_BUDGET: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TreeFingerprint {
    hash: u64,
    sampled: usize,
}

fn fingerprint_tree(root: &Path) -> Option<TreeFingerprint> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let root = root.canonicalize().ok()?;
    if !std::fs::metadata(&root).ok()?.is_dir() {
        return None;
    }
    let mut stack = vec![root.clone()];
    let mut samples: Vec<(PathBuf, bool, u64, u64, u32)> = Vec::new();
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries {
            if samples.len() >= TREE_FINGERPRINT_BUDGET {
                return None;
            }
            let entry = entry.ok()?;
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if metadata.is_dir() {
                stack.push(path.clone());
            }
            let modified = metadata
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            let relative = path.strip_prefix(&root).unwrap_or(&path).to_path_buf();
            samples.push((
                relative,
                metadata.is_dir(),
                metadata.len(),
                modified.as_secs(),
                modified.subsec_nanos(),
            ));
        }
    }
    samples.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = DefaultHasher::new();
    for (relative, is_dir, size, secs, nanos) in &samples {
        relative.hash(&mut hasher);
        is_dir.hash(&mut hasher);
        size.hash(&mut hasher);
        secs.hash(&mut hasher);
        nanos.hash(&mut hasher);
    }
    Some(TreeFingerprint {
        hash: hasher.finish(),
        sampled: samples.len(),
    })
}

/// Extract workspace file paths a tool call reads or writes, for
/// path-precise invalidation. Returns an empty vec when the tool has no
/// parseable path scope (unknown blast radius: bust everything).
fn tool_paths(name: &str, args: &str) -> Vec<String> {
    let parsed: serde_json::Value = match serde_json::from_str(args) {
        Ok(parsed) => parsed,
        Err(_) => return Vec::new(),
    };
    match name {
        "read_file" | "write_file" | "edit_file_hashline" => parsed
            .get("path")
            .and_then(|value| value.as_str())
            .map(|path| vec![path.to_string()])
            .unwrap_or_default(),
        "edit_files_hashline" => parsed
            .get("files")
            .and_then(|value| value.as_array())
            .map(|files| {
                files
                    .iter()
                    .filter_map(|file| file.get("path")?.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Resolve a tool path argument against the workspace root. Falls back to
/// the raw path when there is no work dir — identity comparison only needs
/// both sides resolved the same way.
///
/// Canonicalizes whenever the path exists so symlinked spellings (`/tmp`
/// vs `/private/tmp`, symlinked parents) resolve identically on both the
/// store and invalidate sides; otherwise a write through one spelling would
/// never bust a read cached under the other.
fn resolve_workspace_path(work_dir: Option<&Path>, path: &str) -> PathBuf {
    let joined = match work_dir {
        Some(root) => {
            let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            let candidate = Path::new(path);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                root.join(candidate)
            }
        }
        None => PathBuf::from(path),
    };
    joined.canonicalize().unwrap_or(joined)
}

/// Tools pure enough to serve from cache: deterministic reads whose inputs
/// are guarded against workspace changes.
/// Everything else always executes. Live or mutating tools
/// (window/permission/URL reads, skill loading, screenshots, `evaluate`, all
/// writes/commands) stay out even though that costs re-execution: correctness first.
const CACHEABLE_TOOLS: &[&str] = &[
    "read_file",
    "grep_search",
    "list_dir",
    "get_repo_map",
];

const REPETITION_CACHE_CAP: usize = 64;

const REPETITION_NOTE: &str = "Repeated invocation: identical arguments already ran earlier this turn and produced this same result (served from cache, not re-executed). If you need different information, change the arguments or use another tool.";

impl RepetitionCacheHandle {
    /// Pin the mutation revision before executing a cacheable read.
    fn execution_revision(&self, name: &str) -> Option<u64> {
        if !CACHEABLE_TOOLS.contains(&name) {
            return None;
        }
        self.inner.lock().ok().map(|guard| guard.revision)
    }

    /// Returns the cached content (with steering note), images, and the
    /// original error flag for an identical call that remains current.
    fn lookup(
        &self,
        name: &str,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Option<(ToolOutput, bool)> {
        if !CACHEABLE_TOOLS.contains(&name) {
            return None;
        }
        let key = (name.to_string(), args.to_string());
        let (revision, entry) = {
            let guard = self.inner.lock().ok()?;
            (guard.revision, guard.entries.get(&key)?.clone())
        };
        // External-mutation guard: a file changed outside the tool loop
        // (user edits, watchers, other agents) must not serve stale bytes.
        // Size+mtime both compared: coarse filesystems can share mtimes.
        let fresh = if let Some(path) = entry.path.as_deref() {
            let requested = tool_paths(name, args)
                .into_iter()
                .next()
                .map(|path| resolve_workspace_path(work_dir, &path));
            // Canonical targets also change when a symlink is retargeted.
            requested.as_deref() == Some(path) && fingerprint_file(path) == entry.fingerprint
        } else if let Some((root, sampled)) = entry.tree.as_ref() {
            // Workspace-wide reads (grep/list/map) depend on the whole tree:
            // verify the root and evict on any drift. An unverifiable tree evicts
            // too — serving blind is how stale search results happen.
            work_dir.and_then(|path| path.canonicalize().ok()).as_ref() == Some(root)
                && fingerprint_tree(root) == Some(*sampled)
        } else {
            false
        };
        self.confirm_lookup(&key, revision, &entry, fresh)?;
        Some((
            ToolOutput {
                content: format!("{}\n\n[{REPETITION_NOTE}]", entry.content),
                images: entry.images.clone(),
            },
            entry.is_error,
        ))
    }

    /// Recheck the candidate after unlocked validation. A stale probe must
    /// neither return its old result nor evict a newer entry for the same call.
    fn confirm_lookup(
        &self,
        key: &(String, String),
        revision: u64,
        entry: &Arc<CachedToolResult>,
        fresh: bool,
    ) -> Option<()> {
        let mut guard = self.inner.lock().ok()?;
        if guard.revision != revision
            || !guard
                .entries
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            return None;
        }
        if !fresh {
            guard.entries.remove(key);
            return None;
        }
        Some(())
    }

    fn store(
        &self,
        name: &str,
        args: &str,
        output: &ToolOutput,
        is_error: bool,
        work_dir: Option<&Path>,
        revision: Option<u64>,
    ) {
        if !CACHEABLE_TOOLS.contains(&name) {
            self.invalidate_for_mutation(name, args, work_dir);
            return;
        }
        // Errors cache too: identical error loops are worth short-circuiting,
        // and any later mutation invalidates by revision.
        let Some(revision) = revision else {
            return;
        };
        // The result may predate a concurrent write even when its post-read
        // fingerprint matches the current file. Reject before probing again.
        match self.inner.lock() {
            Ok(guard) if guard.revision == revision => {}
            _ => return,
        }
        let (path, fingerprint): (Option<PathBuf>, Option<FileFingerprint>) = if name == "read_file"
        {
            tool_paths(name, args)
                .into_iter()
                .next()
                .map(|path| {
                    let absolute = resolve_workspace_path(work_dir, &path);
                    let fingerprint = fingerprint_file(&absolute);
                    (Some(absolute), fingerprint)
                })
                .unwrap_or((None, None))
        } else {
            (None, None)
        };
        if name == "read_file" {
            let source = threadlane_tools::read_file_snapshot_path(&output.content)
                .map(|source| resolve_workspace_path(work_dir, &source));
            // Missing paths and fuzzy matches cannot pin the requested
            // file's identity. Re-execute until the resolved path is used.
            if fingerprint.is_none()
                || source
                    .as_ref()
                    .is_some_and(|source| Some(source) != path.as_ref())
            {
                return;
            }
        }
        // Workspace-wide reads pin the complete tree so external edits
        // (outside the tool loop, past the revision guard) bust them.
        let tree: Option<(PathBuf, TreeFingerprint)> = match name {
            "grep_search" | "list_dir" | "get_repo_map" => work_dir.and_then(|root| {
                let canonical = root.canonicalize().ok()?;
                fingerprint_tree(&canonical).map(|sampled| (canonical, sampled))
            }),
            _ => None,
        };
        if matches!(name, "grep_search" | "list_dir" | "get_repo_map") && tree.is_none() {
            return;
        }
        self.store_if_current(
            (name.to_string(), args.to_string()),
            revision,
            Arc::new(CachedToolResult {
                content: output.content.clone(),
                is_error,
                images: output.images.clone(),
                path,
                fingerprint,
                tree,
            }),
        );
    }

    /// A mutation during execution or filesystem probing makes this insertion obsolete.
    fn store_if_current(&self, key: (String, String), revision: u64, entry: Arc<CachedToolResult>) {
        if let Ok(mut guard) = self.inner.lock() {
            if guard.revision != revision {
                return;
            }
            if guard.entries.len() >= REPETITION_CACHE_CAP {
                guard.entries.clear();
            }
            guard.entries.insert(key, entry);
        }
    }

    /// A mutating tool ran: drop precisely what it could have touched.
    /// Path-scoped writes bust the same-path reads plus every workspace-wide
    /// read (grep/list/map depend on whole-tree contents); anything without
    /// parseable paths busts everything. Every mutation advances the revision
    /// so validation/insertion already in progress cannot publish stale work.
    fn invalidate_for_mutation(&self, name: &str, args: &str, work_dir: Option<&Path>) {
        let paths: Vec<PathBuf> = tool_paths(name, args)
            .into_iter()
            .map(|path| resolve_workspace_path(work_dir, &path))
            .collect();
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.revision = guard.revision.wrapping_add(1);
        if paths.is_empty() {
            guard.entries.clear();
            return;
        }
        guard.entries.retain(|(entry_name, _), entry| {
            if entry_name != "read_file" {
                return false;
            }
            match entry.path.as_deref() {
                Some(path) => !paths.iter().any(|touched| touched == path),
                None => false,
            }
        });
    }

    fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.entries.clear();
            guard.revision = guard.revision.wrapping_add(1);
        }
    }
}

struct PreparedToolCall {
    tc: ToolCall,
    arguments: String,
    agent_tool_call: AgentToolCall,
    identity: Option<ToolExecutionIdentity>,
    intent_arguments: Option<String>,
    context: ToolRunContext,
}

enum ToolPreparationFailure {
    Rejected(AgentToolResult),
    Persistence(AgentError),
}

fn tool_persistence_error(call_id: &str, phase: &str, error: impl std::fmt::Display) -> AgentError {
    AgentError::Session(format!(
        "Tool {call_id} {phase} could not be committed: {error}. Preserve the journal and recover the existing operation; do not replay the original call or continue provider execution with an uncommitted reply"
    ))
}

/// Owns the tool executor registry, hook pipeline, and dispatch logic.
///
/// All tool execution methods use `&self` (they clone shared state internally),
/// so the dispatcher can be shared behind an `Arc`.
#[derive(Clone)]
pub struct ToolDispatcher {
    pub(crate) tool_execution_mode: ToolExecutionMode,
    hook_registry: HookRegistry,
    pub tool_intent_recorder: Option<ToolIntentRecorder>,
    pub tool_completion_recorder: Option<ToolCompletionRecorder>,
    pub tool_execution_trace_recorder: Option<crate::provider::ToolExecutionTraceRecorder>,
    pub(crate) allowed_tool_names: Option<HashSet<String>>,
    pub(crate) work_dir: Option<PathBuf>,
    pub(crate) session_id: String,

    tool_executors: Vec<Arc<dyn ToolExecutor>>,
    event_tx: broadcast::Sender<AgentEvent>,
    repetition: RepetitionCacheHandle,
}


impl ToolDispatcher {
    /// Creates a dispatcher backed by the given event channel and hook registry.
    pub(crate) fn new(event_tx: broadcast::Sender<AgentEvent>, hooks: HookRegistry) -> Self {
        Self {
            tool_execution_mode: ToolExecutionMode::Parallel,
            hook_registry: hooks,
            tool_intent_recorder: None,
            tool_completion_recorder: None,
            tool_execution_trace_recorder: None,
            allowed_tool_names: None,
            work_dir: None,
            session_id: String::new(),
            tool_executors: vec![builtin_tool_executor()],
            event_tx,
            repetition: RepetitionCacheHandle::default(),
        }
    }

    /// Drops all cached repetition results and invalidates outstanding ones.
    /// Called once per turn so cached reads can never leak across turns.
    pub(crate) fn clear_repetition_cache(&self) {
        self.repetition.clear();
    }

    // ── Executor registry ─────────────────────────────────────────────

    /// Returns the core and registered executor schemas in provider order,
    /// after conflict deduplication and the active allowlist are applied.
    pub(crate) fn configured_tool_definitions(&self) -> Vec<AgentToolDefinition> {
        let mut definitions = collect_tool_definitions(&self.tool_executors);
        if let Some(allowed) = &self.allowed_tool_names {
            definitions.retain(|d| allowed.contains(&d.name));
        }
        definitions
    }

    pub(crate) fn register_tool_executor(
        &mut self,
        executor: Arc<dyn ToolExecutor>,
    ) -> Result<(), AgentError> {
        let executor_id = executor.executor_id().trim();
        if executor_id.is_empty() {
            return Err(AgentError::ToolRegistration(
                "Tool executor id must not be empty".into(),
            ));
        }
        if self
            .ordered_tool_executors()
            .iter()
            .any(|registered| registered.executor_id() == executor_id)
        {
            return Err(AgentError::ToolRegistration(format!(
                "Tool executor '{executor_id}' is already registered"
            )));
        }

        let mut known_names = HashSet::new();
        for registered in self.ordered_tool_executors() {
            known_names.extend(
                registered
                    .tool_definitions()
                    .iter()
                    .map(|definition| definition.name.clone()),
            );
        }
        for definition in executor.tool_definitions().iter() {
            if definition.name.trim().is_empty() {
                return Err(AgentError::ToolRegistration(format!(
                    "Tool executor '{executor_id}' provided an empty tool name"
                )));
            }
            if !known_names.insert(definition.name.clone()) {
                return Err(AgentError::ToolRegistration(format!(
                    "Tool schema '{}' from executor '{executor_id}' conflicts with an existing schema",
                    definition.name
                )));
            }
        }

        self.tool_executors.push(executor);
        Ok(())
    }

    pub(crate) fn tool_executor_count(&self) -> usize {
        self.ordered_tool_executors().len()
    }

    fn ordered_tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        self.tool_executors.clone()
    }

    // ── Tool execution ────────────────────────────────────────────────

    /// Executes tools and returns results. Intents are recorded before execution.
    pub(crate) async fn execute_tools(
        &self,
        tool_calls: &[ToolCall],
    ) -> Result<Vec<AgentToolResult>, AgentError> {
        self.execute_tools_with_options(tool_calls, self.tool_intent_recorder.clone(), false, false)
            .await
    }

    /// Executes tools without recording intents (e.g., replay).
    #[cfg(test)]
    async fn execute_tools_without_intent_recording(
        &self,
        tool_calls: &[ToolCall],
    ) -> Result<Vec<AgentToolResult>, AgentError> {
        self.execute_tools_with_options(tool_calls, None, false, false)
            .await
    }

    /// Replays already-intended safe tools. The before hook is intentionally
    /// skipped: the durable ToolStarted record is the clearance boundary.
    /// The repetition cache is skipped as well: replay must observe live
    /// state for verification, never cached results.
    pub(crate) async fn execute_tools_for_replay(
        &self,
        tool_calls: &[ToolCall],
    ) -> Result<Vec<AgentToolResult>, AgentError> {
        self.execute_tools_with_options(tool_calls, None, true, true)
            .await
    }

    async fn execute_tools_with_options(
        &self,
        tool_calls: &[ToolCall],
        intent_recorder: Option<ToolIntentRecorder>,
        skip_before_hook: bool,
        skip_repetition_cache: bool,
    ) -> Result<Vec<AgentToolResult>, AgentError> {
        if tool_calls.is_empty() {
            return Ok(Vec::new());
        }
        let mut results = Vec::with_capacity(tool_calls.len());
        let tool_routes: Arc<[ToolExecutorRoute]> = self.tool_execution_routes().await.into();
        let allowed_tool_names = self.allowed_tool_names.clone().map(Arc::new);

        if self.tool_execution_mode == ToolExecutionMode::Sequential {
            for tc in tool_calls {
                let res = self
                    .execute_single_tool(
                        tc,
                        tool_routes.clone(),
                        allowed_tool_names.clone(),
                        intent_recorder.clone(),
                        skip_before_hook,
                        skip_repetition_cache,
                    )
                    .await?;
                results.push(res);
            }
        } else {
            let mut slots: Vec<Option<AgentToolResult>> = vec![None; tool_calls.len()];
            let mut prepared = Vec::new();
            for (index, tc) in tool_calls.iter().enumerate() {
                let context = ToolRunContext {
                    hooks: self.hook_registry.clone(),
                    intent_recorder: intent_recorder.clone(),
                    execution_trace_recorder: self.tool_execution_trace_recorder.clone(),
                    event_tx: self.event_tx.clone(),
                    tool_routes: tool_routes.clone(),
                    allowed_tool_names: allowed_tool_names.clone(),
                    work_dir: self.work_dir.clone(),
                    skip_before_hook,
                    session_id: self.session_id.clone(),
                    repetition: self.repetition.clone(),
                    skip_repetition_cache,
                };
                match Self::prepare_tool_call(tc.clone(), context).await {
                    Ok(call) => prepared.push((index, call)),
                    Err(ToolPreparationFailure::Rejected(result)) => slots[index] = Some(result),
                    Err(ToolPreparationFailure::Persistence(error)) => return Err(error),
                }
            }

            let mut handles = Vec::new();
            for (index, call) in prepared {
                let fallback_call = call.tc.clone();
                let identity = call.identity.clone();
                let handle = AbortOnDrop::new(tokio::spawn(async move {
                    Self::execute_prepared_tool(call).await
                }));
                handles.push((index, fallback_call, identity, handle));
            }

            let mut failure = None;
            for (index, tool_call, identity, handle) in handles {
                let result = match handle.join().await {
                    Ok(Ok(result)) => result,
                    Ok(Err(error)) => {
                        failure.get_or_insert(error);
                        continue;
                    }
                    Err(error) if identity.is_some() => {
                        failure.get_or_insert_with(|| {
                            tool_persistence_error(&tool_call.id, "execution task", error)
                        });
                        continue;
                    }
                    Err(error) => AgentToolResult {
                        tool_call_id: tool_call.id.clone(),
                        name: tool_call.function.name.clone(),
                        content: format!("Tool execution task failed: {error}"),
                        is_error: true,
                        terminate: false,
                        images: Vec::new(),
                    },
                };
                if let Some(recorder) = &self.tool_completion_recorder {
                    if let Err(error) = recorder(&result).await {
                        failure.get_or_insert_with(|| {
                            tool_persistence_error(&result.tool_call_id, "result", error)
                        });
                        continue;
                    }
                    self.acknowledge_tool_reply(identity.as_ref()).await;
                }
                let _ = self.event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    result: result.clone(),
                });
                slots[index] = Some(result);
            }
            // Settle and commit siblings that have already begun before surfacing
            // failure. Dropping their handles could discard another tool's effects.
            if let Some(error) = failure {
                return Err(error);
            }
            results.extend(slots.into_iter().flatten());
        }

        Ok(results)
    }

    async fn execute_single_tool(
        &self,
        tc: &ToolCall,
        tool_routes: Arc<[ToolExecutorRoute]>,
        allowed_tool_names: Option<Arc<HashSet<String>>>,
        intent_recorder: Option<ToolIntentRecorder>,
        skip_before_hook: bool,
        skip_repetition_cache: bool,
    ) -> Result<AgentToolResult, AgentError> {
        let durable_execution = intent_recorder.is_some();
        let result = AssertUnwindSafe(async {
            let call = match Self::prepare_tool_call(
                tc.clone(),
                ToolRunContext {
                    hooks: self.hook_registry.clone(),
                    intent_recorder,
                    execution_trace_recorder: self.tool_execution_trace_recorder.clone(),
                    event_tx: self.event_tx.clone(),
                    tool_routes,
                    allowed_tool_names,
                    work_dir: self.work_dir.clone(),
                    skip_before_hook,
                    session_id: self.session_id.clone(),
                    repetition: self.repetition.clone(),
                    skip_repetition_cache,
                },
            )
            .await
            {
                Ok(call) => call,
                Err(ToolPreparationFailure::Rejected(result)) => return Ok((result, false, None)),
                Err(ToolPreparationFailure::Persistence(error)) => return Err(error),
            };
            let identity = call.identity.clone();
            Self::execute_prepared_tool(call)
                .await
                .map(|result| (result, true, identity))
        })
        .catch_unwind()
        .await;

        let (result, identity) = match result {
            Ok(Ok((result, false, _))) => return Ok(result),
            Ok(Ok((result, true, identity))) => (result, identity),
            Ok(Err(error)) => return Err(error),
            Err(_) if durable_execution => {
                return Err(tool_persistence_error(
                    &tc.id,
                    "execution task",
                    "tool or hook panicked after durable execution began",
                ));
            }
            Err(_) => (
                AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: format!(
                        "Tool '{}' failed: the tool panicked during execution. \
                         Please retry the tool or use another approach.",
                        tc.function.name
                    ),
                    is_error: true,
                    terminate: false,
                    images: Vec::new(),
                },
                None,
            ),
        };
        if let Some(recorder) = &self.tool_completion_recorder {
            recorder(&result)
                .await
                .map_err(|error| tool_persistence_error(&tc.id, "result", error))?;
            self.acknowledge_tool_reply(identity.as_ref()).await;
        }
        let _ = self.event_tx.send(AgentEvent::ToolExecutionEnd {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            result: result.clone(),
        });
        Ok(result)
    }

    pub(crate) async fn acknowledge_tool_reply(&self, identity: Option<&ToolExecutionIdentity>) {
        let Some(identity) = identity else { return };
        for executor in &self.tool_executors {
            if let Err(error) = executor.acknowledge_tool_reply(identity).await {
                // The canonical result is already committed. Preserve its reply;
                // a later reconciliation can retry this storage-only acknowledgment.
                warn!(
                    "saved tool reply {} acknowledgment failed: {error}",
                    identity.tool_call_id
                );
            }
        }
    }

    pub(crate) async fn recover_tool_reply(
        &self,
        call: &ToolCall,
        identity: &ToolExecutionIdentity,
    ) -> Result<Option<AgentToolResult>, AgentError> {
        if !identity.matches_call(&call.id, &call.function.name) {
            return Err(tool_persistence_error(
                &call.id,
                "reply recovery",
                "committed call identity mismatch",
            ));
        }
        let work_dir = self.work_dir.as_deref();
        let (resolved, arguments, dyn_arguments) = resolve_tool_call(call, work_dir);
        for executor in self.ordered_tool_executors() {
            let Some(reply) = executor
                .recover_tool_reply(&resolved, &arguments, work_dir, identity)
                .await
            else {
                continue;
            };
            let (output, is_error) = match reply {
                Ok(RecoveredToolReply::Canonical(result)) => {
                    if !identity.matches_call(&result.tool_call_id, &result.name) {
                        return Err(tool_persistence_error(
                            &call.id,
                            "reply recovery",
                            "saved canonical reply identity mismatch",
                        ));
                    }
                    return Ok(Some(result));
                }
                Ok(RecoveredToolReply::Extension(output)) => (output, false),
                Err(ToolExecutionError::Failed(error)) => (
                    ToolOutput::from(format!("Tool executor error: {error}")),
                    true,
                ),
                Err(ToolExecutionError::RecoveryRequired(error)) => {
                    return Err(tool_persistence_error(&call.id, "reply recovery", error))
                }
            };
            // The VM's terminal output was committed, but host postprocessing may
            // not have begun. Never rerun hooks while delivering that saved output.
            let content = if dyn_arguments.is_some() && !is_error {
                format!(
                    "Exit Status: exit status: 0\n--- STDOUT ---\n{}\n--- STDERR ---",
                    output.content
                )
            } else {
                output.content
            };
            return Ok(Some(AgentToolResult {
                tool_call_id: call.id.clone(),
                name: call.function.name.clone(),
                content,
                is_error,
                terminate: false,
                images: output.images,
            }));
        }
        Ok(None)
    }

    async fn prepare_tool_call(
        tc: ToolCall,
        context: ToolRunContext,
    ) -> Result<PreparedToolCall, ToolPreparationFailure> {
        let (agent_tool_call, arguments, intent_arguments) =
            resolve_tool_call(&tc, context.work_dir.as_deref());

        if context
            .allowed_tool_names
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&agent_tool_call.name))
        {
            let result = AgentToolResult {
                tool_call_id: tc.id.clone(),
                name: tc.function.name.clone(),
                content: format!(
                    "Tool '{}' is not allowed by the current agent policy",
                    agent_tool_call.name
                ),
                is_error: true,
                terminate: false,
                images: Vec::new(),
            };
            let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
                tool_call_id: tc.id,
                name: tc.function.name,
                result: result.clone(),
            });
            return Err(ToolPreparationFailure::Rejected(result));
        }

        if !context.skip_before_hook {
            let hook_ctx = HookContext {
                session_id: context.session_id.clone(),
                lane: "main".into(),
                run_id: None,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(agent_tool_call.name.clone()),
                tool_arguments: Some(arguments.clone()),
                tool_result_content: None,
                tool_result_is_error: None,
                tool_execution_identity: None,
            };
            if let Err(failures) = context.hooks.run_before_tool(&hook_ctx).await {
                let reason = failures
                    .into_iter()
                    .map(|f| format!("{}: {}", f.id, f.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                let res = AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: reason,
                    is_error: true,
                    terminate: false,
                    images: Vec::new(),
                };
                let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    result: res.clone(),
                });
                return Err(ToolPreparationFailure::Rejected(res));
            }
        }

        let identity = if let Some(recorder) = &context.intent_recorder {
            let recorded = recorder(
                &tc.id,
                &tc.function.name,
                intent_arguments.as_deref().unwrap_or(&arguments),
            ).await.and_then(|identity| {
                if identity.matches_call(&tc.id, &tc.function.name) {
                    Ok(identity)
                } else {
                    Err("Committed tool identity does not match the declared call; execution was not started".into())
                }
            });
            match recorded {
                Ok(identity) => Some(identity),
                Err(error) => {
                    return Err(ToolPreparationFailure::Persistence(tool_persistence_error(
                        &tc.id, "intent", error,
                    )));
                }
            }
        } else {
            None
        };

        Ok(PreparedToolCall {
            tc,
            arguments,
            agent_tool_call,
            identity,
            intent_arguments,
            context,
        })
    }

    async fn execute_prepared_tool(call: PreparedToolCall) -> Result<AgentToolResult, AgentError> {
        let PreparedToolCall {
            tc,
            arguments,
            agent_tool_call,
            identity,
            intent_arguments,
            context,
        } = call;
        let start_time = std::time::Instant::now();
        let started_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let executor_kind = context
            .tool_routes
            .iter()
            .find(|route| route.tool_names.contains(&agent_tool_call.name))
            .map(|route| route.executor.executor_id().to_string())
            .unwrap_or_else(|| "unregistered".to_string());
        if let Some(recorder) = &context.execution_trace_recorder {
            if let Err(error) = recorder(crate::provider::ToolExecutionTraceEvent::Started {
                tool_call_id: tc.id.clone(),
                tool_name: tc.function.name.clone(),
                executor_kind: executor_kind.clone(),
                effective_arguments: intent_arguments.as_ref().unwrap_or(&arguments).clone(),
                started_at_ms,
            })
            .await
            {
                return Err(tool_persistence_error(&tc.id, "execution start", error));
            }
        }
        debug!(
            "Tool execution started: '{}' (call_id: {})",
            tc.function.name, tc.id
        );
        let _ = context.event_tx.send(AgentEvent::ToolExecutionStart {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments: intent_arguments.as_ref().unwrap_or(&arguments).clone(),
        });

        let mut execution_result = None;
        let mut reply_executor = None;
        // Error flag for cache hits: the cached content already carries the
        // original error text, so it must not be re-prefixed below.
        let mut cached_is_error = false;
        let mut served_from_cache = false;
        if !context.skip_repetition_cache {
            if let Some((cached, was_error)) = context.repetition.lookup(
                &agent_tool_call.name,
                &arguments,
                context.work_dir.as_deref(),
            ) {
                execution_result = Some(Ok(cached));
                cached_is_error = was_error;
                served_from_cache = true;
            }
        }
        let cache_revision = if !context.skip_repetition_cache && !served_from_cache {
            context.repetition.execution_revision(&agent_tool_call.name)
        } else {
            None
        };
        for route in context.tool_routes.iter() {
            if execution_result.is_some() {
                break;
            }
            if !route.tool_names.contains(&agent_tool_call.name) {
                continue;
            }
            if let Some(result) = route
                .executor
                .execute_tool_with_call(
                    &agent_tool_call,
                    &arguments,
                    context.work_dir.as_deref(),
                    identity.as_ref(),
                )
                .await
            {
                reply_executor = Some(route.executor.clone());
                execution_result = Some(result);
                break;
            }
        }
        let execution_result = execution_result.unwrap_or_else(|| {
            Err(ToolExecutionError::Failed(format!(
                "No registered executor handles tool '{}'. If this is an auxiliary capability, run it via: dyn {} [args]",
                agent_tool_call.name, agent_tool_call.name
            )))
        });
        let (output, is_error) = match execution_result {
            Ok(output) => (output, cached_is_error),
            Err(ToolExecutionError::Failed(error)) => (
                ToolOutput {
                    content: format!("Tool executor error: {error}"),
                    images: Vec::new(),
                },
                true,
            ),
            Err(ToolExecutionError::RecoveryRequired(error)) => {
                return Err(tool_persistence_error(&tc.id, "extension reply", error));
            }
        };
        if !context.skip_repetition_cache && !served_from_cache {
            // Record fresh executions for identical-call dedup, including
            // fresh errors: identical error loops are worth short-circuiting,
            // and any later mutation invalidates the cache. Cache hits never
            // re-store (that would nest steering notes).
            context.repetition.store(
                &agent_tool_call.name,
                &arguments,
                &output,
                is_error,
                context.work_dir.as_deref(),
                cache_revision,
            );
        }
        let duration_ms = start_time.elapsed().as_millis();
        if is_error {
            warn!(
                "Tool execution failed: '{}' (call_id: {}) after {}ms: {}",
                tc.function.name, tc.id, duration_ms, output.content
            );
        } else {
            debug!(
                "Tool execution completed: '{}' (call_id: {}) in {}ms",
                tc.function.name, tc.id, duration_ms
            );
        }
        let mut final_result = AgentToolResult {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            content: output.content,
            is_error,
            terminate: false,
            images: output.images,
        };

        let hook_ctx = HookContext {
            session_id: identity
                .as_ref()
                .map(|identity| identity.session_id.clone())
                .unwrap_or_else(|| context.session_id.clone()),
            lane: identity
                .as_ref()
                .map(|identity| identity.lane.clone())
                .unwrap_or_else(|| "main".into()),
            run_id: identity.as_ref().map(|identity| identity.run_id.clone()),
            tool_call_id: Some(tc.id.clone()),
            tool_name: Some(agent_tool_call.name.clone()),
            tool_arguments: Some(arguments.clone()),
            tool_result_content: Some(final_result.content.clone()),
            tool_result_is_error: Some(final_result.is_error),
            tool_execution_identity: identity.clone(),
        };
        let hook_run = context.hooks.run_after_tool(&hook_ctx).await;
        for failure in hook_run.failures {
            warn!("after-tool hook {} failed: {}", failure.id, failure.message);
        }
        if let Some(content) = hook_run.effect.override_content {
            final_result.content = content;
        }
        if let Some(content) = hook_run.effect.append_content {
            if !content.trim().is_empty() {
                final_result.content.push_str("\n\n");
                final_result.content.push_str(&content);
            }
        }
        if let Some(is_error) = hook_run.effect.override_is_error {
            final_result.is_error = is_error;
        }
        if let Some(terminate) = hook_run.effect.terminate {
            final_result.terminate = terminate;
        }
        if intent_arguments.is_some() && !final_result.is_error {
            final_result.content = format!(
                "Exit Status: exit status: 0\n--- STDOUT ---\n{}\n--- STDERR ---",
                final_result.content,
            );
        }

        if let (Some(identity), Some(executor)) = (identity.as_ref(), reply_executor) {
            executor
                .prepare_tool_reply(identity, &final_result)
                .await
                .map_err(|error| tool_persistence_error(&tc.id, "prepared reply", error))?;
        }

        if let Some(recorder) = &context.execution_trace_recorder {
            if let Err(error) = recorder(crate::provider::ToolExecutionTraceEvent::Finished {
                tool_call_id: final_result.tool_call_id.clone(),
                tool_name: final_result.name.clone(),
                executor_kind: executor_kind.into(),
                started_at_ms,
                duration_ms: start_time.elapsed().as_millis() as u64,
                is_error: final_result.is_error,
                terminate: final_result.terminate,
                output_sha256: format!("{:x}", Sha256::digest(final_result.content.as_bytes())),
                output_bytes: final_result.content.len() as u64,
            })
            .await
            {
                return Err(tool_persistence_error(&tc.id, "execution finish", error));
            }
        }

        Ok(final_result)
    }

    async fn tool_execution_routes(&self) -> Vec<ToolExecutorRoute> {
        let mut claimed_names = HashSet::new();
        self.tool_executors
            .iter()
            .map(|executor| ToolExecutorRoute {
                executor: executor.clone(),
                tool_names: executor
                    .tool_definitions()
                    .iter()
                    .filter_map(|definition| {
                        (!definition.name.trim().is_empty()).then(|| definition.name.clone())
                    })
                    .filter(|name| claimed_names.insert(name.clone()))
                    .collect(),
            })
            .collect()
    }
}

// ── Free functions ───────────────────────────────────────────────────

fn collect_tool_definitions(
    registered_executors: &[Arc<dyn ToolExecutor>],
) -> Vec<AgentToolDefinition> {
    let mut seen = HashSet::new();
    let mut definitions = Vec::new();

    for executor in registered_executors {
        for definition in executor.tool_definitions().iter() {
            if seen.insert(definition.name.clone()) {
                definitions.push(definition.clone());
            }
        }
    }

    definitions
}

fn normalize_tool_arguments(
    name: &str,
    arguments: &str,
    work_dir: Option<&std::path::Path>,
) -> String {
    let Some(work_dir) = work_dir else {
        return arguments.to_string();
    };
    let Ok(mut value) = serde_json::from_str::<Value>(arguments) else {
        return arguments.to_string();
    };
    let workspace = work_dir.to_string_lossy().to_string();
    match (name, value.as_object_mut()) {
        ("read_file" | "write_file" | "edit_file" | "list_dir", Some(object))
            if object
                .get("path")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty) =>
        {
            object.insert("path".into(), Value::String(workspace));
        }
        ("run_command", Some(object))
            if object
                .get("cwd")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty) =>
        {
            object.insert("cwd".into(), Value::String(workspace));
        }
        _ => {}
    }

    serde_json::to_string(&value).unwrap_or_else(|_| arguments.to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HookKind;

    struct StubExecutor {
        id: String,
        tools: Vec<AgentToolDefinition>,
        result: Option<String>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for StubExecutor {
        fn executor_id(&self) -> &str {
            &self.id
        }

        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            self.tools.clone().into()
        }

        async fn execute_tool(&self, _name: &str, _args: &str) -> Option<Result<String, String>> {
            self.result.clone().map(Ok)
        }

        async fn execute_tool_with_call(
            &self,
            call: &AgentToolCall,
            _args: &str,
            _work_dir: Option<&Path>,
            _identity: Option<&ToolExecutionIdentity>,
        ) -> Option<Result<ToolOutput, ToolExecutionError>> {
            // Match by name for the stub.
            if self.tools.iter().any(|d| d.name == call.name) {
                self.result.clone().map(|result| Ok(result.into()))
            } else {
                None
            }
        }
    }

    #[test]
    fn fills_missing_file_paths_from_the_workspace() {
        let arguments =
            normalize_tool_arguments("read_file", "{}", Some(std::path::Path::new("/workspace")));
        assert_eq!(arguments, r#"{"path":"/workspace"}"#);
    }

    fn stub_tool(name: &str) -> AgentToolDefinition {
        AgentToolDefinition::new(
            name,
            "",
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }

    struct CountingExecutor {
        id: String,
        tools: Vec<AgentToolDefinition>,
        result: String,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for CountingExecutor {
        fn executor_id(&self) -> &str {
            &self.id
        }

        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            self.tools.clone().into()
        }

        async fn execute_tool(&self, _name: &str, _args: &str) -> Option<Result<String, String>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Ok(self.result.clone()))
        }
    }

    struct PanickingExecutor;

    #[async_trait::async_trait]
    impl ToolExecutor for PanickingExecutor {
        fn executor_id(&self) -> &str {
            "panicking"
        }

        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            vec![stub_tool("panic_tool")].into()
        }

        async fn execute_tool(&self, _name: &str, _args: &str) -> Option<Result<String, String>> {
            panic!("tool panic")
        }

        async fn execute_tool_with_call(
            &self,
            _call: &AgentToolCall,
            _args: &str,
            _work_dir: Option<&Path>,
            _identity: Option<&ToolExecutionIdentity>,
        ) -> Option<Result<ToolOutput, ToolExecutionError>> {
            panic!("tool panic")
        }
    }

    #[tokio::test]
    async fn sequential_tool_panic_records_completion() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher.tool_execution_mode = ToolExecutionMode::Sequential;
        dispatcher
            .register_tool_executor(Arc::new(PanickingExecutor))
            .unwrap();
        let completions = Arc::new(std::sync::Mutex::new(Vec::new()));
        dispatcher.tool_completion_recorder = Some({
            let completions = completions.clone();
            Arc::new(move |result| {
                let result = result.clone();
                let completions = completions.clone();
                Box::pin(async move {
                    completions.lock().unwrap().push(result);
                    Ok(())
                })
            })
        });

        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "panic_call".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "panic_tool".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await.unwrap();

        assert!(results[0].is_error);
        assert!(results[0].content.contains("panicked during execution"));
        let completions = completions.lock().unwrap();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].tool_call_id, "panic_call");
        assert!(completions[0].is_error);
    }

    fn counting_dispatcher(
        tools: &[(&str, &str)],
    ) -> (
        ToolDispatcher,
        std::collections::HashMap<String, Arc<std::sync::atomic::AtomicUsize>>,
    ) {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        let mut counters = std::collections::HashMap::new();
        // These tests exercise only the counting stubs, including read_file.
        dispatcher.tool_executors.clear();
        for (index, (name, result)) in tools.iter().enumerate() {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            counters.insert(name.to_string(), calls.clone());
            dispatcher
                .register_tool_executor(Arc::new(CountingExecutor {
                    id: format!("stub-{index}"),
                    tools: vec![stub_tool(name)],
                    result: result.to_string(),
                    calls,
                }))
                .expect("register stub");
        }
        (dispatcher, counters)
    }

    fn tool_call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            r#type: "function".into(),
            function: threadlane_protocol::RuntimeToolCallFunction {
                name: name.into(),
                arguments: args.into(),
            },
            thought_signature: None,
        }
    }

    struct CallIdentityExecutor {
        observed: Arc<
            std::sync::Mutex<
                Vec<(
                    AgentToolCall,
                    Option<PathBuf>,
                    Option<ToolExecutionIdentity>,
                )>,
            >,
        >,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for CallIdentityExecutor {
        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            vec![stub_tool("identity_probe")].into()
        }

        async fn execute_tool(&self, _: &str, _: &str) -> Option<Result<String, String>> {
            Some(Err("call identity was dropped".into()))
        }

        async fn execute_tool_with_call(
            &self,
            call: &AgentToolCall,
            args: &str,
            work_dir: Option<&Path>,
            identity: Option<&ToolExecutionIdentity>,
        ) -> Option<Result<ToolOutput, ToolExecutionError>> {
            assert_eq!(call.arguments, args);
            self.observed.lock().unwrap().push((
                call.clone(),
                work_dir.map(Path::to_owned),
                identity.cloned(),
            ));
            Some(Ok(ToolOutput {
                content: call.id.clone(),
                images: vec![identity_image()],
            }))
        }
    }

    #[tokio::test]
    async fn dispatcher_preserves_executor_call_identity() {
        for mode in [ToolExecutionMode::Sequential, ToolExecutionMode::Parallel] {
            let (event_tx, _) = broadcast::channel(8);
            let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
            let directory = tempfile::tempdir().unwrap();
            dispatcher.work_dir = Some(directory.path().to_owned());
            dispatcher.tool_execution_mode = mode;
            let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
            dispatcher
                .register_tool_executor(Arc::new(CallIdentityExecutor {
                    observed: observed.clone(),
                }))
                .unwrap();
            let results = dispatcher
                .execute_tools(&[tool_call(
                    "stable-call",
                    "identity_probe",
                    r#"{"z":1, "a":2}"#,
                )])
                .await
                .unwrap();
            assert_eq!(results[0].tool_call_id, "stable-call");
            assert_eq!(results[0].content, "stable-call");
            assert!(!results[0].is_error);
            assert_eq!(results[0].images, vec![identity_image()]);
            let observed = observed.lock().unwrap();
            assert_eq!(observed.len(), 1);
            assert_eq!(observed[0].0.id, "stable-call");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&observed[0].0.arguments).unwrap(),
                serde_json::json!({"a":2, "z":1}),
            );
            assert!(!observed[0].0.arguments.contains(' '));
            assert_eq!(observed[0].1.as_deref(), Some(directory.path()));
            assert_eq!(observed[0].2, None);
        }
    }

    struct WorkspaceOutputExecutor;

    #[async_trait::async_trait]
    impl ToolExecutor for WorkspaceOutputExecutor {
        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            vec![stub_tool("workspace_output")].into()
        }

        async fn execute_tool(&self, _: &str, _: &str) -> Option<Result<String, String>> {
            Some(Err("workspace and rich output were dropped".into()))
        }

        async fn execute_tool_with_output_in_workspace(
            &self,
            _: &str,
            args: &str,
            work_dir: Option<&Path>,
        ) -> Option<Result<ToolOutput, String>> {
            let directory = work_dir.expect("workspace must reach the existing rich override");
            Some(Ok(ToolOutput {
                content: std::fs::read_to_string(directory.join(args.trim_matches('"'))).unwrap(),
                images: vec![identity_image()],
            }))
        }
    }

    #[tokio::test]
    async fn call_dispatch_retains_existing_workspace_and_rich_output_overrides() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("fixture.txt"), "workspace output").unwrap();
        dispatcher.work_dir = Some(directory.path().to_owned());
        dispatcher
            .register_tool_executor(Arc::new(WorkspaceOutputExecutor))
            .unwrap();
        let results = dispatcher
            .execute_tools(&[tool_call(
                "rich-call",
                "workspace_output",
                r#""fixture.txt""#,
            )])
            .await
            .unwrap();
        assert_eq!(results[0].tool_call_id, "rich-call");
        assert_eq!(results[0].content, "workspace output");
        assert!(!results[0].is_error);
        assert_eq!(results[0].images, vec![identity_image()]);
    }

    fn identity_image() -> ImageAttachment {
        ImageAttachment {
            display_name: "fixture.png".into(),
            data_url: "data:image/png;base64,AA==".into(),
        }
    }

    async fn dyn_preserves_declared_call(mode: ToolExecutionMode) {
        let (event_tx, mut events) = broadcast::channel(8);
        let hooks = HookRegistry::default();
        hooks
            .register(
                HookKind::AfterTool,
                "resolved-tool",
                Arc::new(|context| {
                    Box::pin(async move {
                        assert_eq!(context.session_id, "session");
                        assert_eq!(context.lane, "main");
                        assert_eq!(context.run_id.as_deref(), Some("run"));
                        assert_eq!(context.tool_call_id.as_deref(), Some("declared-call"));
                        assert_eq!(context.tool_name.as_deref(), Some("identity_probe"));
                        assert_eq!(context.tool_arguments.as_deref(), Some(r#"{"x":1}"#));
                        assert_eq!(
                            context.tool_result_content.as_deref(),
                            Some("declared-call")
                        );
                        Ok(crate::harness::HookEffect {
                            append_content: Some("hook reply".into()),
                            ..Default::default()
                        })
                    })
                }),
            )
            .unwrap();
        let mut dispatcher = ToolDispatcher::new(event_tx, hooks);
        dispatcher.tool_execution_mode = mode;
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        dispatcher
            .register_tool_executor(Arc::new(CallIdentityExecutor {
                observed: observed.clone(),
            }))
            .unwrap();
        let intents = Arc::new(std::sync::Mutex::new(Vec::new()));
        dispatcher.tool_intent_recorder = Some({
            let intents = intents.clone();
            Arc::new(move |id, name, args| {
                intents
                    .lock()
                    .unwrap()
                    .push((id.to_owned(), name.to_owned(), args.to_owned()));
                let identity = execution_identity(id, name);
                Box::pin(async move { Ok(identity) })
            })
        });
        let args = serde_json::json!({"command":"dyn identity_probe {\"x\":1}"}).to_string();
        let results = dispatcher
            .execute_tools(&[tool_call("declared-call", "run_command", &args)])
            .await
            .unwrap();
        assert!(!results[0].is_error, "{}", results[0].content);
        assert_eq!(results[0].tool_call_id, "declared-call");
        assert_eq!(results[0].name, "run_command");
        assert!(results[0]
            .content
            .contains("--- STDOUT ---\ndeclared-call\n\nhook reply\n--- STDERR ---"));
        assert_eq!(results[0].images, vec![identity_image()]);
        assert!(
            matches!(events.try_recv().unwrap(), AgentEvent::ToolExecutionStart { tool_call_id, name, arguments }
            if tool_call_id == "declared-call" && name == "run_command" && serde_json::from_str::<Value>(&arguments).unwrap() == serde_json::from_str::<Value>(&args).unwrap())
        );
        assert!(
            matches!(events.try_recv().unwrap(), AgentEvent::ToolExecutionEnd { tool_call_id, name, result }
            if tool_call_id == "declared-call" && name == "run_command" && result == results[0])
        );
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].0.id, "declared-call");
        assert_eq!(observed[0].0.name, "identity_probe");
        assert_eq!(observed[0].0.arguments, r#"{"x":1}"#);
        assert_eq!(
            observed[0].2,
            Some(execution_identity("declared-call", "run_command"))
        );
        let intents = intents.lock().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].0, "declared-call");
        assert_eq!(intents[0].1, "run_command");
        assert_eq!(
            serde_json::from_str::<Value>(&intents[0].2).unwrap(),
            serde_json::from_str::<Value>(&args).unwrap()
        );
    }

    #[tokio::test]
    async fn sequential_dyn_preserves_declared_call() {
        dyn_preserves_declared_call(ToolExecutionMode::Sequential).await;
    }

    #[tokio::test]
    async fn parallel_dyn_preserves_declared_call() {
        dyn_preserves_declared_call(ToolExecutionMode::Parallel).await;
    }

    fn execution_identity(id: &str, name: &str) -> ToolExecutionIdentity {
        ToolExecutionIdentity {
            session_id: "session".into(),
            lane: "main".into(),
            run_id: "run".into(),
            assistant_entry_id: "assistant".into(),
            tool_call_id: id.into(),
            tool_name: name.into(),
            result_entry_id: "result".into(),
        }
    }

    #[tokio::test]
    async fn durable_tool_panic_preserves_intent_and_settles_other_tools() {
        for mode in [ToolExecutionMode::Sequential, ToolExecutionMode::Parallel] {
            let (mut dispatcher, _) = counting_dispatcher(&[("sibling_probe", "sibling reply")]);
            let mut events = dispatcher.event_tx.subscribe();
            dispatcher.tool_execution_mode = mode;
            dispatcher
                .register_tool_executor(Arc::new(PanickingExecutor))
                .unwrap();
            dispatcher.tool_intent_recorder = Some(Arc::new(|id, name, _| {
                let identity = execution_identity(id, name);
                Box::pin(async move { Ok(identity) })
            }));
            let committed = Arc::new(std::sync::Mutex::new(Vec::new()));
            dispatcher.tool_completion_recorder = Some({
                let committed = committed.clone();
                Arc::new(move |result| {
                    let id = result.tool_call_id.clone();
                    let committed = committed.clone();
                    Box::pin(async move {
                        committed.lock().unwrap().push(id);
                        Ok(())
                    })
                })
            });
            let error = dispatcher
                .execute_tools(&[
                    tool_call("panicked", "panic_tool", "{}"),
                    tool_call("sibling", "sibling_probe", "{}"),
                ])
                .await
                .unwrap_err();
            assert!(matches!(error, AgentError::Session(_)), "{error}");
            let expected = if matches!(mode, ToolExecutionMode::Parallel) {
                vec!["sibling".to_string()]
            } else {
                vec![]
            };
            assert_eq!(*committed.lock().unwrap(), expected);
            while let Ok(event) = events.try_recv() {
                assert!(
                    !matches!(event, AgentEvent::ToolExecutionEnd { tool_call_id, .. } if tool_call_id == "panicked")
                );
            }
        }
    }

    #[tokio::test]
    async fn parallel_persistence_failure_settles_and_commits_started_siblings() {
        struct SiblingExecutor {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
            finished: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl ToolExecutor for SiblingExecutor {
            fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
                vec![stub_tool("sibling_probe")].into()
            }
            async fn execute_tool(&self, _: &str, args: &str) -> Option<Result<String, String>> {
                let name = serde_json::from_str::<Value>(args).unwrap()["name"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                if name == "second" {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                self.finished.lock().unwrap().push(name.clone());
                Some(Ok(name))
            }
        }
        for phase in ["finish", "completion"] {
            let (event_tx, mut events) = broadcast::channel(8);
            let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
            dispatcher.tool_execution_mode = ToolExecutionMode::Parallel;
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let finished = Arc::new(std::sync::Mutex::new(Vec::new()));
            dispatcher
                .register_tool_executor(Arc::new(SiblingExecutor {
                    entered: entered.clone(),
                    release: release.clone(),
                    finished: finished.clone(),
                }))
                .unwrap();
            dispatcher.tool_execution_trace_recorder = Some({
                let entered = entered.clone();
                let release = release.clone();
                Arc::new(move |event| {
                    let entered = entered.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        if phase == "finish"
                            && matches!(event, crate::provider::ToolExecutionTraceEvent::Finished { tool_call_id, .. } if tool_call_id == "first")
                        {
                            entered.notified().await;
                            release.notify_one();
                            return Err("first trace commit failed".into());
                        }
                        Ok(())
                    })
                })
            });
            let committed = Arc::new(std::sync::Mutex::new(Vec::new()));
            dispatcher.tool_completion_recorder = Some({
                let committed = committed.clone();
                Arc::new(move |result| {
                    let id = result.tool_call_id.clone();
                    let entered = entered.clone();
                    let release = release.clone();
                    let committed = committed.clone();
                    Box::pin(async move {
                        if phase == "completion" && id == "first" {
                            entered.notified().await;
                            release.notify_one();
                            return Err("first result commit failed".into());
                        }
                        committed.lock().unwrap().push(id);
                        Ok(())
                    })
                })
            });
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                dispatcher.execute_tools(&[
                    tool_call("first", "sibling_probe", r#"{"name":"first"}"#),
                    tool_call("second", "sibling_probe", r#"{"name":"second"}"#),
                ]),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(error.to_string().contains("first"));
            assert_eq!(finished.lock().unwrap().as_slice(), ["first", "second"]);
            assert_eq!(committed.lock().unwrap().as_slice(), ["second"]);
            let mut published = Vec::new();
            while let Ok(event) = events.try_recv() {
                if let AgentEvent::ToolExecutionEnd { tool_call_id, .. } = event {
                    published.push(tool_call_id);
                }
            }
            assert_eq!(published, ["second"]);
        }
    }

    #[tokio::test]
    async fn persistence_failure_does_not_publish_a_tool_completion() {
        for mode in [ToolExecutionMode::Sequential, ToolExecutionMode::Parallel] {
            for phase in ["intent", "start", "finish", "completion"] {
                let (event_tx, mut events) = broadcast::channel(8);
                let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
                dispatcher.tool_execution_mode = mode;
                let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                dispatcher
                    .register_tool_executor(Arc::new(CountingExecutor {
                        id: "effect-probe".into(),
                        tools: vec![stub_tool("effect_probe")],
                        result: "effect performed".into(),
                        calls: calls.clone(),
                    }))
                    .unwrap();
                dispatcher.tool_intent_recorder = Some(Arc::new(move |id, name, _| {
                    let identity = execution_identity(id, name);
                    Box::pin(async move {
                        if phase == "intent" {
                            Err("intent storage failed".into())
                        } else {
                            Ok(identity)
                        }
                    })
                }));
                dispatcher.tool_execution_trace_recorder = Some(Arc::new(move |event| {
                    Box::pin(async move {
                        match (phase, event) {
                            ("start", crate::provider::ToolExecutionTraceEvent::Started { .. })
                            | (
                                "finish",
                                crate::provider::ToolExecutionTraceEvent::Finished { .. },
                            ) => Err("trace storage failed".into()),
                            _ => Ok(()),
                        }
                    })
                }));
                let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                dispatcher.tool_completion_recorder = Some({
                    let completions = completions.clone();
                    Arc::new(move |_| {
                        completions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Box::pin(async move {
                            if phase == "completion" {
                                Err("result storage failed".into())
                            } else {
                                Ok(())
                            }
                        })
                    })
                });
                let error = dispatcher
                    .execute_tools(&[tool_call("call", "effect_probe", "{}")])
                    .await
                    .unwrap_err();
                assert!(matches!(error, AgentError::Session(_)));
                assert!(error.to_string().contains("recover the existing operation"));
                assert_eq!(
                    calls.load(std::sync::atomic::Ordering::SeqCst),
                    usize::from(matches!(phase, "finish" | "completion"))
                );
                assert_eq!(
                    completions.load(std::sync::atomic::Ordering::SeqCst),
                    usize::from(phase == "completion")
                );
                while let Ok(event) = events.try_recv() {
                    assert!(
                        !matches!(event, AgentEvent::ToolExecutionEnd { .. }),
                        "mode {mode:?}, phase {phase}: published an uncommitted completion"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn invalid_committed_identity_prevents_execution() {
        for mode in [ToolExecutionMode::Sequential, ToolExecutionMode::Parallel] {
            for invalid in [
                "call",
                "tool",
                "session",
                "lane",
                "run",
                "assistant",
                "result",
            ] {
                let (event_tx, _) = broadcast::channel(8);
                let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
                dispatcher.tool_execution_mode = mode;
                let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
                dispatcher
                    .register_tool_executor(Arc::new(CallIdentityExecutor {
                        observed: observed.clone(),
                    }))
                    .unwrap();
                let mut identity = execution_identity("call", "identity_probe");
                match invalid {
                    "call" => identity.tool_call_id = "other".into(),
                    "tool" => identity.tool_name = "other".into(),
                    "session" => identity.session_id.clear(),
                    "lane" => identity.lane.clear(),
                    "run" => identity.run_id.clear(),
                    "assistant" => identity.assistant_entry_id.clear(),
                    "result" => identity.result_entry_id = " ".into(),
                    _ => unreachable!(),
                }
                dispatcher.tool_intent_recorder = Some(Arc::new(move |_, _, _| {
                    let identity = identity.clone();
                    Box::pin(async move { Ok(identity) })
                }));
                let results = dispatcher
                    .execute_tools(&[tool_call("call", "identity_probe", "{}")])
                    .await
                    .unwrap_err();
                assert!(
                    results.to_string().contains("execution was not started"),
                    "mode {mode:?}, invalid {invalid}"
                );
                assert!(observed.lock().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn dyn_resolves_quoted_objects_and_preserves_shell_fallbacks() {
        for args in [r#"{"x":1}"#, r#"'{"x":1}'"#, r#""{"x":1}""#] {
            assert_eq!(
                dyn_tool_call(
                    &serde_json::json!({"command":format!("dyn identity_probe {args}")})
                        .to_string()
                ),
                Some(("identity_probe".into(), r#"{"x":1}"#.into()))
            );
        }
        for command in [
            "dyn identity_probe --help",
            "dyn identity_probe -h",
            "dyn identity_probe {} | cat",
            "dyn identity_probe []",
            "dyn identity_probe invalid",
        ] {
            assert_eq!(
                dyn_tool_call(&serde_json::json!({"command":command}).to_string()),
                None
            );
        }
    }

    #[tokio::test]
    async fn dyn_cannot_bypass_the_resolved_tool_policy() {
        for mode in [ToolExecutionMode::Sequential, ToolExecutionMode::Parallel] {
            let (event_tx, _) = broadcast::channel(8);
            let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
            dispatcher.tool_execution_mode = mode;
            dispatcher.allowed_tool_names = Some(HashSet::from(["run_command".into()]));
            let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
            dispatcher
                .register_tool_executor(Arc::new(CallIdentityExecutor {
                    observed: observed.clone(),
                }))
                .unwrap();
            dispatcher.tool_intent_recorder = Some(Arc::new(|_, _, _| {
                panic!("policy rejection must precede intent commitment")
            }));
            let args = serde_json::json!({"command":"dyn identity_probe '{}'"}).to_string();
            let results = dispatcher
                .execute_tools(&[tool_call("call", "run_command", &args)])
                .await
                .unwrap();
            assert!(results[0].is_error);
            assert!(results[0]
                .content
                .contains("'identity_probe' is not allowed"));
            assert_eq!(results[0].tool_call_id, "call");
            assert_eq!(results[0].name, "run_command");
            assert!(observed.lock().unwrap().is_empty());
        }
    }

    fn call_count(
        counters: &std::collections::HashMap<String, Arc<std::sync::atomic::AtomicUsize>>,
        name: &str,
    ) -> usize {
        counters[name].load(std::sync::atomic::Ordering::SeqCst)
    }

    struct PausedBuiltinRead {
        name: &'static str,
        captured: tokio::sync::Notify,
        release: tokio::sync::Notify,
        reads: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for PausedBuiltinRead {
        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            crate::tool_executor::BuiltinToolExecutor.tool_definitions()
        }

        async fn execute_tool(&self, _: &str, _: &str) -> Option<Result<String, String>> {
            panic!("paused read requires its workspace")
        }

        async fn execute_tool_in_workspace(
            &self,
            name: &str,
            args: &str,
            work_dir: Option<&Path>,
        ) -> Option<Result<String, String>> {
            let result = crate::tool_executor::BuiltinToolExecutor
                .execute_tool_in_workspace(name, args, work_dir)
                .await;
            if name == self.name
                && self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
            {
                self.captured.notify_one();
                self.release.notified().await;
            }
            result
        }
    }

    #[tokio::test]
    async fn reads_finishing_after_a_tool_write_cannot_cache_old_contents() {
        for mode in [ToolExecutionMode::Sequential, ToolExecutionMode::Parallel] {
            for (name, args) in [
                ("read_file", r#"{"path":"a.rs"}"#),
                ("grep_search", r#"{"pattern":"marker"}"#),
                ("list_dir", r#"{"path":"."}"#),
                ("get_repo_map", "{}"),
            ] {
                let directory = tempfile::tempdir().unwrap();
                std::fs::write(directory.path().join("a.rs"), "fn before_marker() {}\n").unwrap();
                let executor = Arc::new(PausedBuiltinRead {
                    name,
                    captured: tokio::sync::Notify::new(),
                    release: tokio::sync::Notify::new(),
                    reads: std::sync::atomic::AtomicUsize::new(0),
                });
                let (event_tx, _) = broadcast::channel(8);
                let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
                dispatcher.work_dir = Some(directory.path().to_owned());
                dispatcher.tool_execution_mode = mode;
                dispatcher.tool_executors.clear();
                dispatcher.register_tool_executor(executor.clone()).unwrap();
                let dispatcher = Arc::new(dispatcher);
                let pending = tokio::spawn({
                    let dispatcher = dispatcher.clone();
                    async move {
                        dispatcher
                            .execute_tools(&[tool_call("before", name, args)])
                            .await
                            .unwrap()
                    }
                });
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    executor.captured.notified(),
                )
                .await
                .expect("read must capture its result before the write");
                // Complete a real tool write, including cache invalidation,
                // while the original read still holds its earlier result.
                let target = if name == "read_file" { "a.rs" } else { "b.rs" };
                let write_args =
                    serde_json::json!({"path":target,"content":"fn after_marker() {}\n"})
                        .to_string();
                let written = dispatcher
                    .execute_tools(&[tool_call("write", "write_file", &write_args)])
                    .await
                    .unwrap();
                assert!(!written[0].is_error, "{}", written[0].content);
                executor.release.notify_one();
                let before = pending.await.unwrap();
                assert!(!before[0].is_error, "{}", before[0].content);
                let expected =
                    threadlane_tools::try_execute_tool_in_workspace(name, args, directory.path())
                        .unwrap();
                assert_ne!(before[0].content, expected, "{mode:?}: {name}");
                let after = dispatcher
                    .execute_tools(&[tool_call("after", name, args)])
                    .await
                    .unwrap();
                assert_eq!(
                    after[0].content, expected,
                    "{mode:?}: {name} reused a read from before the write"
                );
                let cached = dispatcher
                    .execute_tools(&[tool_call("cached", name, args)])
                    .await
                    .unwrap();
                assert!(cached[0].content.starts_with(&expected));
                assert!(cached[0].content.contains("served from cache"));
                assert_eq!(executor.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
            }
        }
    }

    struct LiveStateExecutor {
        state: Arc<std::sync::Mutex<String>>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for LiveStateExecutor {
        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            [
                "computer_status",
                "computer_windows",
                "browser_current_url",
                "load_skill",
            ]
            .into_iter()
            .map(stub_tool)
            .collect::<Vec<_>>()
            .into()
        }

        async fn execute_tool(&self, _: &str, _: &str) -> Option<Result<String, String>> {
            Some(Ok(self.state.lock().unwrap().clone()))
        }
    }

    #[tokio::test]
    async fn live_state_reads_observe_external_changes_without_a_mutating_tool() {
        for mode in [ToolExecutionMode::Sequential, ToolExecutionMode::Parallel] {
            for (name, before, after) in [
                (
                    "computer_status",
                    "permission missing",
                    "permission granted",
                ),
                ("computer_windows", "window 1", "window 2"),
                ("load_skill", "skill source valid", "skill source changed"),
                (
                    "browser_current_url",
                    "https://example.com/old",
                    "https://example.com/new",
                ),
            ] {
                let (event_tx, _) = broadcast::channel(8);
                let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
                dispatcher.tool_execution_mode = mode;
                let state = Arc::new(std::sync::Mutex::new(before.to_string()));
                dispatcher
                    .register_tool_executor(Arc::new(LiveStateExecutor {
                        state: state.clone(),
                    }))
                    .unwrap();
                let first = dispatcher
                    .execute_tools(&[tool_call("first", name, "{}")])
                    .await
                    .unwrap();
                assert_eq!(first[0].content, before);
                // User actions and app navigation bypass tool-loop invalidation.
                *state.lock().unwrap() = after.to_string();
                let second = dispatcher
                    .execute_tools(&[tool_call("second", name, "{}")])
                    .await
                    .unwrap();
                assert_eq!(second[0].content, after, "{mode:?}: {name}");
            }
        }
    }

    #[tokio::test]
    async fn repetition_cache_serves_identical_reads_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("read.txt");
        std::fs::write(&path, "file content").unwrap();
        let args = serde_json::json!({"path": path}).to_string();
        let (dispatcher, counters) =
            counting_dispatcher(&[("read_file", "file content"), ("browser_act", "ok")]);
        let call = tool_call("call-1", "read_file", &args);
        let first = dispatcher.execute_tools(&[call.clone()]).await.unwrap();
        let second = dispatcher.execute_tools(&[call]).await.unwrap();
        assert_eq!(call_count(&counters, "read_file"), 1);
        assert_eq!(first[0].content, "file content");
        assert!(
            second[0].content.contains("served from cache"),
            "cache hit must steer the model: {}",
            second[0].content
        );
        assert!(!second[0].is_error);
    }

    #[tokio::test]
    async fn mutation_busts_the_repetition_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("read.txt");
        std::fs::write(&path, "file content").unwrap();
        let args = serde_json::json!({"path": path}).to_string();
        let (dispatcher, counters) =
            counting_dispatcher(&[("read_file", "file content"), ("browser_act", "ok")]);
        let read = tool_call("call-1", "read_file", &args);
        let write = tool_call("call-2", "browser_act", "{}");
        dispatcher.execute_tools(&[read.clone()]).await.unwrap();
        dispatcher.execute_tools(&[write]).await.unwrap();
        dispatcher.execute_tools(&[read]).await.unwrap();
        assert_eq!(call_count(&counters, "read_file"), 2);
        assert_eq!(call_count(&counters, "browser_act"), 1);
    }

    #[tokio::test]
    async fn cached_read_errors_keep_their_flag_and_clear_after_a_write() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("read.txt"), [0xff]).unwrap();
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher.work_dir = Some(directory.path().to_owned());
        let args = r#"{"path":"read.txt"}"#;
        let first = dispatcher
            .execute_tools(&[tool_call("first", "read_file", args)])
            .await
            .unwrap();
        let cached = dispatcher
            .execute_tools(&[tool_call("cached-error", "read_file", args)])
            .await
            .unwrap();
        assert!(first[0].is_error);
        assert!(cached[0].is_error);
        assert!(cached[0].content.starts_with(&first[0].content));
        assert_eq!(cached[0].content.matches("Tool executor error:").count(), 1);
        assert_eq!(cached[0].content.matches("served from cache").count(), 1);
        let written = dispatcher
            .execute_tools(&[tool_call(
                "write",
                "write_file",
                r#"{"path":"read.txt","content":"recovered"}"#,
            )])
            .await
            .unwrap();
        assert!(!written[0].is_error);
        let fresh = dispatcher
            .execute_tools(&[tool_call("fresh", "read_file", args)])
            .await
            .unwrap();
        assert!(!fresh[0].is_error);
        assert!(fresh[0].content.contains("recovered"));
        assert!(!fresh[0].content.contains("served from cache"));
        let cached = dispatcher
            .execute_tools(&[tool_call("cached-success", "read_file", args)])
            .await
            .unwrap();
        assert!(!cached[0].is_error);
        assert!(cached[0].content.starts_with(&fresh[0].content));
        assert_eq!(cached[0].content.matches("served from cache").count(), 1);
    }

    #[tokio::test]
    async fn mutating_tools_always_execute() {
        let (dispatcher, counters) = counting_dispatcher(&[("browser_act", "ok")]);
        let call = tool_call("call-1", "browser_act", "{}");
        dispatcher.execute_tools(&[call.clone()]).await.unwrap();
        dispatcher.execute_tools(&[call]).await.unwrap();
        assert_eq!(call_count(&counters, "browser_act"), 2);
    }

    #[tokio::test]
    async fn clearing_resets_the_repetition_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("read.txt");
        std::fs::write(&path, "file content").unwrap();
        let args = serde_json::json!({"path": path}).to_string();
        let (dispatcher, counters) = counting_dispatcher(&[("read_file", "file content")]);
        let call = tool_call("call-1", "read_file", &args);
        dispatcher.execute_tools(&[call.clone()]).await.unwrap();
        dispatcher.clear_repetition_cache();
        dispatcher.execute_tools(&[call]).await.unwrap();
        assert_eq!(call_count(&counters, "read_file"), 2);
    }

    #[tokio::test]
    async fn replay_skips_the_repetition_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("read.txt");
        std::fs::write(&path, "file content").unwrap();
        let args = serde_json::json!({"path": path}).to_string();
        let (dispatcher, counters) = counting_dispatcher(&[("read_file", "file content")]);
        let call = tool_call("call-1", "read_file", &args);
        dispatcher.execute_tools(&[call.clone()]).await.unwrap();
        dispatcher.execute_tools_for_replay(&[call]).await.unwrap();
        assert_eq!(call_count(&counters, "read_file"), 2);
    }

    #[test]
    fn tool_paths_extracts_read_and_write_scopes() {
        assert_eq!(
            tool_paths("read_file", r#"{"path":"src/a.rs"}"#),
            vec!["src/a.rs".to_string()]
        );
        assert_eq!(
            tool_paths(
                "edit_files_hashline",
                r#"{"files":[{"path":"x.rs"},{"path":"y.rs"}]}"#
            ),
            vec!["x.rs".to_string(), "y.rs".to_string()]
        );
        assert!(tool_paths("run_command", r#"{"command":"ls"}"#).is_empty());
        assert!(tool_paths("read_file", "not-json").is_empty());
    }

    #[test]
    fn same_path_write_busts_only_that_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "A").unwrap();
        std::fs::write(dir.path().join("b.rs"), "B").unwrap();
        let work_dir = Some(dir.path());
        let cache = RepetitionCacheHandle::default();
        let output = |text: &str| ToolOutput {
            content: text.into(),
            images: Vec::new(),
        };
        cache.store(
            "read_file",
            r#"{"path":"a.rs"}"#,
            &output("A"),
            false,
            work_dir,
            cache.execution_revision("read_file"),
        );
        cache.store(
            "read_file",
            r#"{"path":"b.rs"}"#,
            &output("B"),
            false,
            work_dir,
            cache.execution_revision("read_file"),
        );
        cache.store(
            "grep_search",
            r#"{"pattern":"x"}"#,
            &output("G"),
            false,
            work_dir,
            cache.execution_revision("grep_search"),
        );
        // Same-path write busts read_file(a) plus all workspace-wide reads.
        cache.invalidate_for_mutation("write_file", r#"{"path":"a.rs"}"#, work_dir);
        assert!(cache
            .lookup("read_file", r#"{"path":"a.rs"}"#, work_dir)
            .is_none());
        assert!(cache
            .lookup("read_file", r#"{"path":"b.rs"}"#, work_dir)
            .is_some());
        assert!(cache
            .lookup("grep_search", r#"{"pattern":"x"}"#, work_dir)
            .is_none());
    }

    #[test]
    fn external_modification_busts_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("watched.rs");
        std::fs::write(&file, "version one!!!!").unwrap();
        let args = r#"{"path":"watched.rs"}"#;
        let work_dir = Some(dir.path());
        let cache = RepetitionCacheHandle::default();
        cache.store(
            "read_file",
            args,
            &ToolOutput {
                content: "version one!!!!".into(),
                images: Vec::new(),
            },
            false,
            work_dir,
            cache.execution_revision("read_file"),
        );
        assert!(cache.lookup("read_file", args, work_dir).is_some());
        // External edit (different size forces detection even on filesystems
        // with coarse mtime granularity).
        std::fs::write(&file, "version two, changed").unwrap();
        assert!(
            cache.lookup("read_file", args, work_dir).is_none(),
            "externally modified file must re-execute"
        );
    }

    #[tokio::test]
    async fn fuzzy_file_reads_observe_external_changes_and_new_candidates() {
        for initially_present in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("src")).unwrap();
            let source = dir.path().join("src/state.rs");
            if initially_present {
                std::fs::write(&source, "old source\n").unwrap();
            }
            let (event_tx, _) = broadcast::channel(8);
            let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
            dispatcher.work_dir = Some(dir.path().to_path_buf());
            let args = r#"{"path":"crates/removed/state.rs"}"#;
            let first = dispatcher
                .execute_tools(&[tool_call("first", "read_file", args)])
                .await
                .unwrap();
            assert_eq!(first[0].is_error, !initially_present);
            if initially_present {
                assert!(first[0].content.contains("old source"));
                assert!(first[0].content.contains("Auto-resolved"));
            }
            // This edit bypasses the tool loop's mutation invalidation.
            std::fs::write(&source, "fresh source version\n").unwrap();
            let second = dispatcher
                .execute_tools(&[tool_call("second", "read_file", args)])
                .await
                .unwrap();
            assert!(!second[0].is_error, "{}", second[0].content);
            assert!(
                second[0].content.contains("fresh source version"),
                "{}",
                second[0].content
            );
            assert!(!second[0].content.contains("served from cache"));
            std::fs::create_dir_all(dir.path().join("crates/removed")).unwrap();
            std::fs::write(
                dir.path().join("crates/removed/state.rs"),
                "exact path source\n",
            )
            .unwrap();
            let exact = dispatcher
                .execute_tools(&[tool_call("exact", "read_file", args)])
                .await
                .unwrap();
            assert!(
                exact[0].content.contains("exact path source"),
                "{}",
                exact[0].content
            );
            let cached = dispatcher
                .execute_tools(&[tool_call("cached", "read_file", args)])
                .await
                .unwrap();
            assert!(cached[0].content.contains("served from cache"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retargeted_symlink_reads_observe_the_current_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "original source\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "different source\n").unwrap();
        let alias = dir.path().join("alias.rs");
        std::os::unix::fs::symlink("a.rs", &alias).unwrap();
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher.work_dir = Some(dir.path().to_path_buf());
        let args = r#"{"path":"alias.rs"}"#;
        let first = dispatcher
            .execute_tools(&[tool_call("first", "read_file", args)])
            .await
            .unwrap();
        assert!(first[0].content.contains("original source"));
        let cached = dispatcher
            .execute_tools(&[tool_call("cached", "read_file", args)])
            .await
            .unwrap();
        assert!(cached[0].content.contains("served from cache"));
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("b.rs", &alias).unwrap();
        let changed = dispatcher
            .execute_tools(&[tool_call("changed", "read_file", args)])
            .await
            .unwrap();
        assert!(!changed[0].is_error, "{}", changed[0].content);
        assert!(
            changed[0].content.contains("different source"),
            "{}",
            changed[0].content
        );
        assert!(!changed[0].content.contains("served from cache"));
    }

    #[test]
    fn deleted_file_busts_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gone.rs");
        std::fs::write(&file, "here").unwrap();
        let args = r#"{"path":"gone.rs"}"#;
        let work_dir = Some(dir.path());
        let cache = RepetitionCacheHandle::default();
        cache.store(
            "read_file",
            args,
            &ToolOutput {
                content: "here".into(),
                images: Vec::new(),
            },
            false,
            work_dir,
            cache.execution_revision("read_file"),
        );
        std::fs::remove_file(&file).unwrap();
        assert!(cache.lookup("read_file", args, work_dir).is_none());
    }

    #[tokio::test]
    async fn dispatcher_executes_registered_tool() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher
            .register_tool_executor(Arc::new(StubExecutor {
                id: "stub".into(),
                tools: vec![stub_tool("hello")],
                result: Some("world".into()),
            }))
            .unwrap();

        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "hello".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "world");
        assert!(!results[0].is_error);
    }

    #[tokio::test]
    async fn dispatcher_records_physical_execution_envelope_once() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher
            .register_tool_executor(Arc::new(StubExecutor {
                id: "stub".into(),
                tools: vec![stub_tool("hello")],
                result: Some("world".into()),
            }))
            .unwrap();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder_observed = observed.clone();
        dispatcher.tool_execution_trace_recorder = Some(Arc::new(move |event| {
            let observed = recorder_observed.clone();
            Box::pin(async move {
                observed.lock().unwrap().push(event);
                Ok(())
            })
        }));

        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "hello".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await.unwrap();

        assert!(!results[0].is_error);
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 2);
        assert!(matches!(
            &observed[0],
            crate::provider::ToolExecutionTraceEvent::Started {
                tool_call_id,
                executor_kind,
                ..
            } if tool_call_id == "call_1" && executor_kind == "stub"
        ));
        assert!(matches!(
            &observed[1],
            crate::provider::ToolExecutionTraceEvent::Finished {
                tool_call_id,
                output_bytes: 5,
                output_sha256,
                ..
            } if tool_call_id == "call_1" && output_sha256.len() == 64
        ));
    }

    #[tokio::test]
    async fn builtins_are_registered_in_the_unified_executor_registry() {
        let (event_tx, _) = broadcast::channel(8);
        let dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());

        assert_eq!(dispatcher.tool_executor_count(), 1);
        assert!(dispatcher
            .configured_tool_definitions()
            .iter()
            .any(|definition| definition.name == "read_file"));
    }

    #[tokio::test]
    async fn empty_tool_batches_do_not_enumerate_executors() {
        struct NoEnumeration;
        #[async_trait::async_trait]
        impl ToolExecutor for NoEnumeration {
            fn executor_id(&self) -> &str {
                "no-enumeration"
            }
            fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
                panic!("empty batches must not enumerate tool schemas")
            }
            async fn execute_tool(&self, _: &str, _: &str) -> Option<Result<String, String>> {
                panic!("empty batches must not execute tools")
            }
        }
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher.tool_executors = vec![Arc::new(NoEnumeration)];
        for mode in [ToolExecutionMode::Sequential, ToolExecutionMode::Parallel] {
            dispatcher.tool_execution_mode = mode;
            assert!(dispatcher.execute_tools(&[]).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn dispatcher_rejects_duplicate_registration() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        let exec = Arc::new(StubExecutor {
            id: "dup".into(),
            tools: vec![stub_tool("a")],
            result: None,
        });
        dispatcher.register_tool_executor(exec.clone()).unwrap();
        assert!(dispatcher.register_tool_executor(exec).is_err());
    }

    #[tokio::test]
    async fn before_tool_hook_can_block_execution() {
        let (event_tx, _) = broadcast::channel(8);
        let hooks = HookRegistry::default();
        hooks
            .replace(
                HookKind::BeforeTool,
                "blocker",
                Arc::new(|_ctx| Box::pin(async move { Err("blocked by test".into()) })),
            )
            .unwrap();

        let mut dispatcher = ToolDispatcher::new(event_tx, hooks);
        dispatcher
            .register_tool_executor(Arc::new(StubExecutor {
                id: "stub".into(),
                tools: vec![stub_tool("stub_write")],
                result: Some("written".into()),
            }))
            .unwrap();

        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "stub_write".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await.unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0].content.contains("blocked by test"));
    }

    #[tokio::test]
    async fn unknown_tool_is_rejected_without_a_registered_route() {
        let (event_tx, _) = broadcast::channel(8);
        let dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "nonexistent_tool_xyz".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await.unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0]
            .content
            .contains("No registered executor handles tool"));
    }

    #[test]
    fn registered_tools_are_visible_by_default_and_allowlist_filters_them() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());

        let defs = dispatcher.configured_tool_definitions();
        assert!(defs.iter().any(|d| d.name == "list_dir"));
        assert!(defs.iter().any(|d| d.name == "grep_search"));
        assert!(defs.iter().any(|d| d.name == "manage_memory"));

        dispatcher.allowed_tool_names = Some(HashSet::from(["list_dir".to_string()]));
        let filtered = dispatcher.configured_tool_definitions();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "list_dir");
    }
}

#[cfg(test)]
mod cache_freshness_tests {
    use super::*;

    fn output(text: &str) -> ToolOutput {
        ToolOutput {
            content: text.into(),
            images: Vec::new(),
        }
    }

    #[test]
    fn mutations_and_clear_fence_in_flight_cache_probes() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.rs"), "A").unwrap();
        std::fs::write(directory.path().join("b.rs"), "B").unwrap();
        let root = Some(directory.path());
        let args = r#"{"path":"a.rs"}"#;
        let key = ("read_file".to_string(), args.to_string());
        let other_args = r#"{"path":"b.rs"}"#;
        let other_key = ("read_file".to_string(), other_args.to_string());
        for mutation in ["write_file", "run_command", "clear"] {
            let cache = RepetitionCacheHandle::default();
            cache.store(
                "read_file",
                args,
                &output("A"),
                false,
                root,
                cache.execution_revision("read_file"),
            );
            cache.store(
                "read_file",
                other_args,
                &output("B"),
                false,
                root,
                cache.execution_revision("read_file"),
            );
            // Pause between the lookup/store filesystem probe and its commit.
            let (revision, entry, other_entry) = {
                let guard = cache.inner.lock().unwrap();
                (
                    guard.revision,
                    guard.entries[&key].clone(),
                    guard.entries[&other_key].clone(),
                )
            };
            assert!(cache.confirm_lookup(&key, revision, &entry, true).is_some());
            if mutation == "clear" {
                cache.clear();
            } else {
                cache.invalidate_for_mutation(mutation, args, root);
            }
            assert!(cache.confirm_lookup(&key, revision, &entry, true).is_none());
            cache.store_if_current(key.clone(), revision, entry);
            assert!(cache.lookup("read_file", args, root).is_none());
            // Even an unrelated mutation fences a probe already in progress;
            // its failed validation must not evict a surviving unrelated read.
            assert!(cache
                .confirm_lookup(&other_key, revision, &other_entry, false)
                .is_none());
            assert_eq!(
                cache.lookup("read_file", other_args, root).is_some(),
                mutation == "write_file"
            );
            cache.store(
                "read_file",
                args,
                &output("fresh"),
                false,
                root,
                cache.execution_revision("read_file"),
            );
            assert!(cache
                .lookup("read_file", args, root)
                .unwrap()
                .0
                .content
                .starts_with("fresh\n"));
        }
    }

    #[test]
    fn old_validation_cannot_return_or_evict_a_replacement() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.rs"), "A").unwrap();
        let root = Some(directory.path());
        let args = r#"{"path":"a.rs"}"#;
        let key = ("read_file".to_string(), args.to_string());
        let cache = RepetitionCacheHandle::default();
        cache.store(
            "read_file",
            args,
            &output("old"),
            false,
            root,
            cache.execution_revision("read_file"),
        );
        let (revision, old) = {
            let guard = cache.inner.lock().unwrap();
            (guard.revision, guard.entries[&key].clone())
        };
        cache.store(
            "read_file",
            args,
            &output("replacement"),
            false,
            root,
            cache.execution_revision("read_file"),
        );
        for fresh in [true, false] {
            assert!(cache.confirm_lookup(&key, revision, &old, fresh).is_none());
            let current = cache.lookup("read_file", args, root).unwrap().0;
            assert!(current.content.starts_with("replacement\n"));
        }
    }

    #[tokio::test]
    async fn list_dir_observes_external_empty_directory_changes() {
        let directory = tempfile::tempdir().unwrap();
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher.work_dir = Some(directory.path().to_path_buf());
        let call = ToolCall {
            id: "listing".into(),
            r#type: "function".into(),
            function: threadlane_protocol::RuntimeToolCallFunction {
                name: "list_dir".into(),
                arguments: r#"{"path":"."}"#.into(),
            },
            thought_signature: None,
        };
        let first = dispatcher.execute_tools(&[call.clone()]).await.unwrap();
        assert_eq!(first[0].content, "");
        let cached = dispatcher.execute_tools(&[call.clone()]).await.unwrap();
        assert!(cached[0].content.contains("served from cache"));

        let empty = directory.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let created = dispatcher.execute_tools(&[call.clone()]).await.unwrap();
        assert_eq!(created[0].content, "[DIR]  empty");
        let renamed = directory.path().join("renamed");
        std::fs::rename(empty, &renamed).unwrap();
        let moved = dispatcher.execute_tools(&[call.clone()]).await.unwrap();
        assert_eq!(moved[0].content, "[DIR]  renamed");
        std::fs::remove_dir(renamed).unwrap();
        let removed = dispatcher.execute_tools(&[call]).await.unwrap();
        assert_eq!(removed[0].content, "");
    }

    #[test]
    fn incomplete_tree_fingerprints_never_enable_blind_cache_hits() {
        for directories_only in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            for index in 0..TREE_FINGERPRINT_BUDGET {
                let path = directory.path().join(index.to_string());
                if directories_only {
                    std::fs::create_dir(path).unwrap();
                } else {
                    std::fs::write(path, "content").unwrap();
                }
            }
            assert_eq!(
                fingerprint_tree(directory.path()).map(|tree| tree.sampled),
                Some(TREE_FINGERPRINT_BUDGET)
            );
            let overflow = directory.path().join("overflow");
            if directories_only {
                std::fs::create_dir(overflow).unwrap();
            } else {
                std::fs::write(overflow, "content").unwrap();
            }
            assert!(fingerprint_tree(directory.path()).is_none());
            let cache = RepetitionCacheHandle::default();
            for (name, args) in [
                ("list_dir", r#"{"path":"."}"#),
                ("grep_search", r#"{"pattern":"content"}"#),
                ("get_repo_map", "{}"),
            ] {
                cache.store(
                    name,
                    args,
                    &output("old"),
                    false,
                    Some(directory.path()),
                    cache.execution_revision(name),
                );
                assert!(cache.lookup(name, args, Some(directory.path())).is_none());
            }
        }
        let cache = RepetitionCacheHandle::default();
        cache.store(
            "list_dir",
            "{}",
            &output("old"),
            false,
            None,
            cache.execution_revision("list_dir"),
        );
        assert!(cache.lookup("list_dir", "{}", None).is_none());
    }

    #[test]
    fn unverifiable_workspace_roots_are_not_cached() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        std::fs::write(&file, "content").unwrap();
        for root in [file, directory.path().join("missing")] {
            assert!(fingerprint_tree(&root).is_none());
            let cache = RepetitionCacheHandle::default();
            cache.store(
                "list_dir",
                "{}",
                &output("old"),
                false,
                Some(&root),
                cache.execution_revision("list_dir"),
            );
            assert!(cache.lookup("list_dir", "{}", Some(&root)).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn retargeted_workspace_root_busts_tree_cache() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&first, &alias).unwrap();
        let cache = RepetitionCacheHandle::default();
        cache.store(
            "list_dir",
            "{}",
            &output("first"),
            false,
            Some(&alias),
            cache.execution_revision("list_dir"),
        );
        assert!(cache.lookup("list_dir", "{}", Some(&alias)).is_some());
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(second, &alias).unwrap();
        assert!(cache.lookup("list_dir", "{}", Some(&alias)).is_none());
    }

    #[test]
    fn external_tree_edit_busts_workspace_wide_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "aaa").unwrap();
        let work_dir = Some(dir.path());
        let cache = RepetitionCacheHandle::default();
        cache.store(
            "list_dir",
            r#"{"path":"."}"#,
            &output("a.rs"),
            false,
            work_dir,
            cache.execution_revision("list_dir"),
        );
        assert!(cache
            .lookup("list_dir", r#"{"path":"."}"#, work_dir)
            .is_some());
        // External change anywhere in the tree (new file, same root mtime
        // granularity aside) must not serve the stale listing.
        std::fs::write(dir.path().join("b.rs"), "bbb").unwrap();
        assert!(
            cache
                .lookup("list_dir", r#"{"path":"."}"#, work_dir)
                .is_none(),
            "externally changed tree must re-execute list_dir"
        );
    }

    #[test]
    fn untouched_tree_keeps_workspace_wide_cache() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "aaa").unwrap();
        let work_dir = Some(dir.path());
        let cache = RepetitionCacheHandle::default();
        cache.store(
            "grep_search",
            r#"{"pattern":"aaa"}"#,
            &output("a.rs:1:aaa"),
            false,
            work_dir,
            cache.execution_revision("grep_search"),
        );
        assert!(cache
            .lookup("grep_search", r#"{"pattern":"aaa"}"#, work_dir)
            .is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_spellings_resolve_identically() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("a.rs"), "aaa").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        let via_real = resolve_workspace_path(Some(&real), "a.rs");
        let via_link = resolve_workspace_path(Some(&dir.path().join("link")), "a.rs");
        assert_eq!(via_real, via_link);
    }
}
