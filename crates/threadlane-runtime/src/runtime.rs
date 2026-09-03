//! Single, unified agent runtime.
//!
//! [`AgentRuntime`] is the sole agent execution engine. It owns provider
//! routing, tool dispatch, the harness durability layer, and the turn loop.
//! It replaces the previous split between [`UnifiedAgent`] and
//! [`ProviderRunExecutor`].

use crate::compaction::{compact_messages_to_token_budget, should_auto_compact};
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::events::AgentEvent;
use crate::harness::{
    AgentHarness, HarnessEventHub, HookRegistry, JsonlStore, ProcedureError, ProvisionedEntry,
    QueueKind, Reducer, SessionStore,
};
use crate::tool_dispatcher::ToolDispatcher;
use crate::types::{
    AgentMessage, AgentToolDefinition, AgentToolResult, ImageAttachment, TokenUsage,
    ToolExecutionMode, TurnState,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_protocol::{DeferredResponse, ProviderPort, RuntimeToolCall as ToolCall};
use tokio::sync::{broadcast, Mutex};

/// Unified source for model-visible context.
///
/// Durable callers should provide the canonical session projection. The
/// legacy closure aliases below remain available for compatibility, but new
/// integrations should use this single source instead of coordinating several
/// precedence-based callbacks.
pub trait ModelContextSource: Send + Sync {
    fn project(&self) -> Result<Vec<AgentMessage>, String>;
}

impl<F> ModelContextSource for F
where
    F: Fn() -> Result<Vec<AgentMessage>, String> + Send + Sync,
{
    fn project(&self) -> Result<Vec<AgentMessage>, String> {
        self()
    }
}

pub type ModelContextProjector = Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>;

/// The single, unified agent runtime.
///
/// Owns the harness (durable session store), provider routing, tool dispatch,
/// and the turn loop. This replaces both `UnifiedAgent` and
/// `ProviderRunExecutor`.
pub struct AgentRuntime {
    /// Durable session journal.
    harness: AgentHarness<JsonlStore>,
    /// Tool dispatch with hook-based routing.
    pub tool_dispatcher: ToolDispatcher,
    /// Provider port for API calls.
    provider_client: Arc<dyn ProviderPort>,
    /// In-memory working copy of turn state. The harness is authoritative;
    /// this copy is refreshed from the canonical store before each turn.
    pub turn: Arc<Mutex<TurnState>>,
    /// Agent configuration (compaction, stream rules, model roles, etc.).
    config: AgentConfig,
    /// API key for the active provider.
    pub api_key: String,
    /// Optional account ID for the active provider.
    pub account_id: Option<String>,
    /// Session identifier.
    pub session_id: String,
    /// Working directory for tool execution.
    pub work_dir: Option<PathBuf>,
    /// Event broadcast channel.
    pub event_tx: broadcast::Sender<AgentEvent>,
    /// Hook registry for before/after tool hooks.
    pub hook_registry: HookRegistry,
    /// Steering queue — high-priority prompts injected mid-turn.
    steering_queue: Vec<AgentMessage>,
    /// Follow-up queue — appends to turn after completion.
    follow_up_queue: Vec<AgentMessage>,
    /// Compiled stream rules for runtime monitoring.
    stream_rules: Vec<(crate::rules::StreamRule, regex::Regex)>,
    /// Prompt cache key for provider-side caching.
    prompt_cache_key: Option<String>,
    /// Optional allowlist of tool names.
    allowed_tool_names: Option<HashSet<String>>,
    /// Provider trace recorder (for auditing).
    provider_trace_recorder: Option<crate::provider::ProviderTraceRecorder>,
    /// Asynchronous context preparation at the provider-attempt boundary.
    provider_boundary_preparer: Option<crate::provider::ProviderBoundaryPreparer>,
    /// Assistant message recorder (for persistence).
    message_recorder: Option<crate::provider::AssistantMessageRecorder>,
    /// Harness event hub for wiring durability events.
    pub harness_event_hub: HarnessEventHub,
}

impl AgentRuntime {
    // ── Construction ──────────────────────────────────────────────────

    /// Create a new runtime directly backed by an existing [`AgentHarness`].
    ///
    /// The runtime shares the harness's store, hooks, and event hub directly.

    pub fn from_harness_with_provider(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
        harness: AgentHarness<JsonlStore>,
        config: AgentConfig,
        provider_client: Arc<dyn ProviderPort>,
    ) -> Self {
        let api_key: String = api_key.into();
        let model = model.into();
        let (event_tx, _) = broadcast::channel(config.event_channel_capacity);
        let harness_event_hub = harness.events().clone();
        let hooks = harness.hooks().clone();
        let mut tool_dispatcher = ToolDispatcher::new(event_tx.clone(), hooks.clone());
        tool_dispatcher.core_tool_schema_mode = config.core_tool_schema_mode;
        let turn = Arc::new(Mutex::new(TurnState {
            system_prompt: config.default_system_prompt.clone(),
            messages: Vec::new(),
            model,
            reasoning_effort: Default::default(),
        }));

        Self {
            harness,
            tool_dispatcher,
            provider_client,
            turn,
            config,
            api_key,
            account_id,
            session_id: String::new(),
            work_dir: None,
            event_tx,
            harness_event_hub,
            hook_registry: hooks,
            steering_queue: Vec::new(),
            follow_up_queue: Vec::new(),
            stream_rules: Vec::new(),
            prompt_cache_key: None,
            allowed_tool_names: None,
            provider_trace_recorder: None,
            provider_boundary_preparer: None,
            message_recorder: None,
        }
    }

    /// Create a new runtime backed by the given session journal path.
    ///
    /// If `session_file` is provided, opens (or creates) a JSONL journal.
    /// Otherwise, an in-memory store is used.
    pub fn new(
        _api_key: impl Into<String>,
        _account_id: Option<String>,
        _model: impl Into<String>,
        session_file: Option<&Path>,
        config: AgentConfig,
    ) -> Result<Self, AgentError> {
        let store = if let Some(path) = session_file {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            if !path.exists() {
                std::fs::File::create(path)
                    .map_err(|e| AgentError::Session(format!("create session file: {e}")))?;
            }
            JsonlStore::open(path)
                .map_err(|e| AgentError::Session(format!("open session journal: {e}")))?
        } else {
            // Ephemeral store backed by a temp file.
            let tmp =
                std::env::temp_dir().join(format!("threadlane-ephemeral-{}", std::process::id()));
            let _ = std::fs::create_dir_all(tmp.parent().unwrap());
            JsonlStore::open(&tmp)
                .map_err(|e| AgentError::Session(format!("open ephemeral journal: {e}")))?
        };

        let harness_event_hub = HarnessEventHub::new(config.event_channel_capacity);
        let _harness = AgentHarness::with_events(store, harness_event_hub);
        Err(AgentError::Session(
            "AgentRuntime requires an injected ProviderPort; use new_with_provider".into(),
        ))
    }

    pub fn new_with_provider(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
        session_file: Option<&Path>,
        config: AgentConfig,
        provider_client: Arc<dyn ProviderPort>,
    ) -> Result<Self, AgentError> {
        let store = if let Some(path) = session_file {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            if !path.exists() {
                std::fs::File::create(path)
                    .map_err(|e| AgentError::Session(format!("create session file: {e}")))?;
            }
            JsonlStore::open(path)
                .map_err(|e| AgentError::Session(format!("open session journal: {e}")))?
        } else {
            let tmp =
                std::env::temp_dir().join(format!("threadlane-ephemeral-{}", std::process::id()));
            JsonlStore::open(&tmp)
                .map_err(|e| AgentError::Session(format!("open ephemeral journal: {e}")))?
        };
        let harness_event_hub = HarnessEventHub::new(config.event_channel_capacity);
        let harness = AgentHarness::with_events(store, harness_event_hub);
        Ok(Self::from_harness_with_provider(
            api_key,
            account_id,
            model,
            harness,
            config,
            provider_client,
        ))
    }

    // ── Model context ─────────────────────────────────────────────────

    /// Refreshes the in-memory turn messages directly from the authoritative harness projection.
    pub async fn refresh_projected_messages(&self) {
        if let Ok(messages) = self.projected_messages().await {
            self.turn.lock().await.messages = messages;
        }
    }

    /// Returns the canonical messages from the main harness projection.
    pub async fn projected_messages(&self) -> Result<Vec<AgentMessage>, AgentError> {
        self.projected_messages_on_lane("main").await
    }

    /// Returns the canonical messages from a specific harness lane projection.
    pub async fn projected_messages_on_lane(
        &self,
        lane: &str,
    ) -> Result<Vec<AgentMessage>, AgentError> {
        let context = self
            .harness
            .store()
            .model_context(lane)
            .map_err(|error| AgentError::Session(error.to_string()))?;
        let system_prompt = self.turn.lock().await.system_prompt.clone();
        Ok(std::iter::once(AgentMessage::System {
            content: system_prompt,
        })
        .chain(context.messages())
        .collect())
    }

    /// Syncs the in-memory turn state from the canonical main harness projection.
    pub async fn sync_turn_from_model_context(&self) -> Result<(), AgentError> {
        self.sync_turn_from_model_context_on_lane("main").await
    }

    /// Syncs the in-memory turn state from a specific canonical harness lane.
    pub async fn sync_turn_from_model_context_on_lane(&self, lane: &str) -> Result<(), AgentError> {
        let messages = self.projected_messages_on_lane(lane).await?;
        let mut turn = self.turn.lock().await;
        turn.messages = messages;
        Ok(())
    }

    /// Read the durable transcript projection for UI reconciliation and audit
    /// views. Must not be used to build a provider request.
    pub fn transcript_projection(&self) -> crate::harness::TranscriptProjection {
        self.harness.store().transcript("main")
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    pub fn model(&self) -> String {
        self.turn
            .try_lock()
            .map(|t| t.model.clone())
            .unwrap_or_else(|_| {
                self.harness
                    .store()
                    .model()
                    .unwrap_or_else(|| "gpt-4o".to_string())
            })
    }

    pub fn steer(&mut self, message: AgentMessage) {
        let seq = self.harness.store().next_sequence();
        let persisted = self.harness.enqueue_unbound(
            QueueKind::Steer,
            ProvisionedEntry {
                id: format!("queued-steer-{seq}"),
                parent_id: None,
                message: message.clone(),
                surface_op: crate::harness::SurfaceOperation::Append,
            },
        );
        let persisted = persisted.and_then(|_| {
            self.harness
                .drive_to_completion()
                .map_err(ProcedureError::from)
        });
        match persisted {
            Ok(()) => self.steering_queue.push(message),
            Err(error) => {
                let _ = self.event_tx.send(AgentEvent::AgentError {
                    error: format!("failed to persist steering: {error}"),
                });
            }
        }
    }

    pub fn follow_up(&mut self, message: AgentMessage) {
        let seq = self.harness.store().next_sequence();
        let persisted = self.harness.enqueue_unbound(
            QueueKind::FollowUp,
            ProvisionedEntry {
                id: format!("queued-followup-{seq}"),
                parent_id: None,
                message: message.clone(),
                surface_op: crate::harness::SurfaceOperation::Append,
            },
        );
        let persisted = persisted.and_then(|_| {
            self.harness
                .drive_to_completion()
                .map_err(ProcedureError::from)
        });
        match persisted {
            Ok(()) => self.follow_up_queue.push(message),
            Err(error) => {
                let _ = self.event_tx.send(AgentEvent::AgentError {
                    error: format!("failed to persist follow-up: {error}"),
                });
            }
        }
    }

    /// Records a user prompt through the harness then runs the turn loop.
    pub async fn prompt_message(&mut self, message: AgentMessage) {
        let run_id = format!("foreground-{}", self.harness.store().next_sequence());
        if let Err(error) = self.harness.accept_prompt(&run_id, message) {
            let _ = self.event_tx.send(AgentEvent::AgentError {
                error: format!("failed to accept prompt before provider work: {error}"),
            });
            return;
        }
        if let Err(error) = self.harness.drive_to_completion() {
            let _ = self.event_tx.send(AgentEvent::AgentError {
                error: format!("failed to commit prompt before provider work: {error}"),
            });
            return;
        }
        let accepted_through_seq = self.harness.store().next_sequence().saturating_sub(1);
        self.run_accepted(&run_id, "main", accepted_through_seq)
            .await;
    }

    /// Prompt shorthand (user message with no images).
    pub async fn prompt(&mut self, text: &str) {
        self.prompt_message(AgentMessage::user(text, Vec::new()))
            .await;
    }

    /// Execute a turn loop for a pre-accepted durable run token.
    pub async fn run_accepted(&mut self, run_id: &str, lane: &str, accepted_through_seq: u64) {
        let validation = self
            .harness
            .store_mut()
            .ensure_fresh()
            .map_err(|error| error.to_string())
            .and_then(|_| {
                self.harness
                    .validate_accepted_run_token(run_id, lane, accepted_through_seq)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = validation {
            let _ = self.event_tx.send(AgentEvent::AgentError {
                error: format!("refusing unvalidated accepted run: {error}"),
            });
            return;
        }
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        let usage = self.run_turns().await;
        let _ = self.event_tx.send(AgentEvent::AgentEnd { usage });
    }

    /// Resumes provider/tool execution without appending a duplicate prompt.
    pub async fn resume_pending_turn(&mut self) {
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        let usage = self.run_turns().await;
        let _ = self.event_tx.send(AgentEvent::AgentEnd { usage });
    }

    pub fn set_credentials(&mut self, api_key: String, account_id: Option<String>) {
        self.api_key = api_key;
        self.account_id = account_id;
    }

    pub fn set_prompt_cache_key(&mut self, key: Option<String>) {
        self.prompt_cache_key = key;
    }

    pub fn prompt_cache_enabled(&self) -> bool {
        self.prompt_cache_key.is_some()
    }

    pub fn set_provider_trace_recorder(
        &mut self,
        recorder: Option<crate::provider::ProviderTraceRecorder>,
    ) {
        self.provider_trace_recorder = recorder;
    }

    pub fn set_provider_boundary_preparer(
        &mut self,
        preparer: Option<crate::provider::ProviderBoundaryPreparer>,
    ) {
        self.provider_boundary_preparer = preparer;
    }

    pub fn set_message_recorder(
        &mut self,
        recorder: Option<crate::provider::AssistantMessageRecorder>,
    ) {
        self.message_recorder = recorder;
    }

    pub fn set_model_roles(&mut self, roles: crate::types::ModelRoles) {
        self.config.model_roles = roles;
    }

    pub fn set_needle_enabled(&mut self, enabled: bool) {
        self.config.needle_enabled = enabled;
    }

    pub fn model_roles(&self) -> &crate::types::ModelRoles {
        &self.config.model_roles
    }

    pub fn provider_client(&self) -> &dyn ProviderPort {
        self.provider_client.as_ref()
    }

    pub fn provider_client_arc(&self) -> Arc<dyn ProviderPort> {
        self.provider_client.clone()
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    pub fn state_clone(&self) -> Arc<Mutex<TurnState>> {
        self.turn.clone()
    }

    pub async fn get_state(&self) -> TurnState {
        self.turn.lock().await.clone()
    }

    /// Returns the current system prompt.
    pub fn system_prompt(&self) -> String {
        self.turn
            .try_lock()
            .map(|t| t.system_prompt.clone())
            .unwrap_or_else(|_| "".to_string())
    }

    /// Returns the current reasoning effort.
    pub fn reasoning_effort(&self) -> crate::types::ReasoningEffort {
        self.turn
            .try_lock()
            .map(|t| t.reasoning_effort)
            .unwrap_or_else(|_| Default::default())
    }

    /// Returns a snapshot of the current messages.
    pub async fn messages(&self) -> Vec<AgentMessage> {
        self.turn.lock().await.messages.clone()
    }

    pub fn register_tool_executor(
        &mut self,
        executor: Arc<dyn crate::tool_executor::ToolExecutor>,
    ) -> Result<(), AgentError> {
        self.tool_dispatcher.register_tool_executor(executor)
    }

    pub fn configured_tool_definitions(&self) -> Vec<AgentToolDefinition> {
        self.tool_dispatcher.configured_tool_definitions()
    }

    pub fn set_allowed_tool_names(&mut self, names: Option<HashSet<String>>) {
        self.allowed_tool_names = names.clone();
        self.tool_dispatcher.allowed_tool_names = names;
    }

    pub fn set_stream_rules(&mut self, rules: Vec<crate::rules::StreamRule>) {
        self.stream_rules = rules
            .into_iter()
            .filter_map(|r| regex::Regex::new(&r.pattern).ok().map(|re| (r, re)))
            .collect();
    }

    pub fn tool_executor_count(&self) -> usize {
        self.tool_dispatcher.tool_executor_count()
    }

    pub async fn set_reasoning_effort(&self, effort: crate::types::ReasoningEffort) {
        self.turn.lock().await.reasoning_effort = effort;
    }

    pub async fn set_system_prompt(&self, prompt: String) {
        let mut turn = self.turn.lock().await;
        turn.system_prompt = prompt.clone();
        if let Some(AgentMessage::System { content }) = turn.messages.first_mut() {
            *content = prompt;
        } else {
            turn.messages
                .insert(0, AgentMessage::System { content: prompt });
        }
    }

    /// Computes manual compaction without mutating provider context. Durable
    /// callers commit this projection before installing it in memory.
    pub async fn preview_compact_history(
        &self,
        options: Option<crate::compaction::CompactionOptions>,
    ) -> Vec<AgentMessage> {
        let turn = self.turn.lock().await;
        match options {
            Some(opts) => crate::compaction::compact_messages(&turn.messages, &opts),
            None => {
                let by_tokens = compact_messages_to_token_budget(
                    &turn.messages,
                    self.config.auto_compaction_keep_recent_tokens,
                );
                if by_tokens.len() == turn.messages.len() {
                    crate::compaction::compact_messages(
                        &turn.messages,
                        &crate::compaction::CompactionOptions::default(),
                    )
                } else {
                    by_tokens
                }
            }
        }
    }

    pub async fn compact_history(
        &self,
        options: Option<crate::compaction::CompactionOptions>,
    ) -> bool {
        let compacted = self.preview_compact_history(options).await;
        let mut turn = self.turn.lock().await;
        let changed = compacted != turn.messages;
        turn.messages = compacted;
        changed
    }

    pub async fn auto_compact_history(&self) -> bool {
        let mut turn = self.turn.lock().await;
        if !should_auto_compact(&turn.messages, &self.config) {
            return false;
        }
        let compacted = compact_messages_to_token_budget(
            &turn.messages,
            self.config.auto_compaction_keep_recent_tokens,
        );
        let changed = compacted.len() != turn.messages.len();
        turn.messages = compacted;
        changed
    }

    pub async fn execute_tools_for_replay(&self, calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.synced_dispatcher()
            .execute_tools_for_replay(calls)
            .await
    }

    pub async fn execute_tools(&self, calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.synced_dispatcher().execute_tools(calls).await
    }

    pub async fn run_steer(&mut self) {
        if !self.steering_queue.is_empty() {
            let items = self.steering_queue.clone();
            if let Some(recorder) = self.message_recorder.as_ref() {
                for item in &items {
                    if let Err(error) = recorder(item.clone()).await {
                        let _ = self.event_tx.send(AgentEvent::AgentError {
                            error: format!(
                                "failed to persist steering before provider work: {error}"
                            ),
                        });
                        return;
                    }
                }
            }
            self.steering_queue.clear();
            {
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }
            self.run_turns().await;
        }
    }

    pub async fn run_follow_up(&mut self) {
        if !self.follow_up_queue.is_empty() {
            let items = self.follow_up_queue.clone();
            if let Some(recorder) = self.message_recorder.as_ref() {
                for item in &items {
                    if let Err(error) = recorder(item.clone()).await {
                        let _ = self.event_tx.send(AgentEvent::AgentError {
                            error: format!(
                                "failed to persist follow-up before provider work: {error}"
                            ),
                        });
                        return;
                    }
                }
            }
            self.follow_up_queue.clear();
            {
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }
            self.run_turns().await;
        }
    }

    pub async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<DeferredResponse, String> {
        self.provider_client.fetch_deferred(model, handle_id).await
    }

    pub async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String> {
        self.provider_client.cancel_deferred(model, handle_id).await
    }

    // ── Harness accessors ─────────────────────────────────────────────

    pub fn harness(&self) -> &AgentHarness<JsonlStore> {
        &self.harness
    }

    pub fn harness_mut(&mut self) -> &mut AgentHarness<JsonlStore> {
        &mut self.harness
    }

    pub fn drive_harness(&mut self) -> Result<(), ProcedureError> {
        self.harness
            .drive_to_completion()
            .map_err(ProcedureError::Effects)
    }

    pub fn enqueue_harness_queue(
        &mut self,
        queue: QueueKind,
        content: String,
        images: Vec<ImageAttachment>,
    ) -> Result<String, String> {
        let state = Reducer::reduce(self.harness.store()).map_err(|error| error.to_string())?;
        let lane = state
            .lane("main")
            .ok_or_else(|| "main harness lane is unavailable".to_string())?;
        let parent_id = lane.leaf_id.clone();
        let seq = self.harness.store().entries().len() as u64 + 1;
        let entry_id = format!("queued-{seq}");
        self.harness
            .enqueue_unbound(
                queue,
                ProvisionedEntry {
                    id: entry_id.clone(),
                    parent_id,
                    message: AgentMessage::user(content, images),
                    surface_op: crate::harness::SurfaceOperation::Append,
                },
            )
            .map_err(|error| error.to_string())?;
        self.harness
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(entry_id)
    }

    pub fn consume_harness_queue(&mut self, queue: QueueKind) -> Result<(), String> {
        let state = Reducer::reduce(self.harness.store()).map_err(|error| error.to_string())?;
        let queued = state
            .lane("main")
            .map(|lane| {
                lane.queued
                    .iter()
                    .filter(|entry| entry.queue == queue)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for entry in queued {
            self.harness
                .consume_unbound(&entry.target.id)
                .map_err(|error| error.to_string())?;
        }
        self.harness
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn cancel_harness_queue_entry(&mut self, entry_id: &str) -> Result<(), String> {
        self.harness
            .cancel_unbound(entry_id)
            .map_err(|error| error.to_string())?;
        self.harness
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────

    fn synced_dispatcher(&self) -> ToolDispatcher {
        let mut td = self.tool_dispatcher.clone();
        td.tool_execution_mode = ToolExecutionMode::Parallel;
        td.work_dir = self.work_dir.clone();
        td.session_id = self.session_id.clone();
        td.allowed_tool_names = self.allowed_tool_names.clone();
        td
    }

    /// Runs the main turn loop.
    async fn run_turns(&mut self) -> TokenUsage {
        let tool_dispatcher = self.synced_dispatcher();
        let mut driver = crate::turn_driver::TurnDriver {
            turn: self.turn.clone(),
            provider_client: self.provider_client.clone(),
            prompt_cache_key: self.prompt_cache_key.clone(),
            tool_dispatcher,
            config: self.config.clone(),
            event_tx: self.event_tx.clone(),
            harness_event_hub: self.harness_event_hub.clone(),
            provider_trace_recorder: self.provider_trace_recorder.clone(),
            provider_boundary_preparer: self.provider_boundary_preparer.clone(),
            message_recorder: self.message_recorder.clone(),
            stream_rules: self.stream_rules.clone(),
            steering_queue: &mut self.steering_queue,
            follow_up_queue: &mut self.follow_up_queue,
        };
        driver.run_turns().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sha2::Digest;
    use threadlane_protocol::{
        RuntimeRequest, RuntimeStreamEvent, RuntimeToolCall as ToolCall,
        RuntimeToolCallFunction as ToolCallFunction, RuntimeUsage,
    };

    use crate::provider::{ProviderBoundaryRequest, ProviderBoundaryResult};

    struct UnusedProvider;

    #[async_trait]
    impl ProviderPort for UnusedProvider {
        async fn stream_request(
            &self,
            _request: RuntimeRequest,
            _events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
        }

        async fn fetch_deferred(
            &self,
            _model: &str,
            _handle_id: &str,
        ) -> Result<DeferredResponse, String> {
            Ok(DeferredResponse::Pending)
        }

        async fn cancel_deferred(&self, _model: &str, _handle_id: &str) -> Result<(), String> {
            Ok(())
        }

        fn provider_kind(&self, _model: &str) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn projected_messages_can_target_a_dedicated_lane() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let store = JsonlStore::open(&path).unwrap();
        let mut harness = AgentHarness::new(store);
        harness
            .accept_prompt_and_drive_on_lane(
                "subagent-worker",
                "child-run",
                AgentMessage::user("child task", Vec::new()),
            )
            .unwrap();
        drop(harness);
        let runtime = AgentRuntime::new_with_provider(
            "",
            None,
            "test-model",
            Some(&path),
            AgentConfig::default(),
            Arc::new(UnusedProvider),
        )
        .unwrap();

        let messages = runtime
            .projected_messages_on_lane("subagent-worker")
            .await
            .unwrap();

        assert!(messages.iter().any(|message| matches!(
            message,
            AgentMessage::User { content } if content == "child task"
        )));
    }

    struct RecordingProvider {
        order: Arc<std::sync::Mutex<Vec<&'static str>>>,
        message_counts: Arc<std::sync::Mutex<Vec<usize>>>,
        request_tools: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait]
    impl ProviderPort for RecordingProvider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            self.order.lock().unwrap().push("sent");
            self.message_counts
                .lock()
                .unwrap()
                .push(request.messages.as_array().map_or(0, Vec::len));
            self.request_tools
                .lock()
                .unwrap()
                .push(request.tools.clone());
            let _ = events
                .send(RuntimeStreamEvent::ContentToken("done".into()))
                .await;
            let _ = events
                .send(RuntimeStreamEvent::Finished {
                    tool_calls: Vec::new(),
                    usage: RuntimeUsage::default(),
                })
                .await;
        }

        async fn fetch_deferred(
            &self,
            _model: &str,
            _handle_id: &str,
        ) -> Result<DeferredResponse, String> {
            Ok(DeferredResponse::Pending)
        }

        async fn cancel_deferred(&self, _model: &str, _handle_id: &str) -> Result<(), String> {
            Ok(())
        }

        fn provider_kind(&self, _model: &str) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn public_run_accepted_rejects_non_durable_token_before_provider_send() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingProvider {
            order: order.clone(),
            message_counts: Arc::new(std::sync::Mutex::new(Vec::new())),
            request_tools: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let mut runtime = AgentRuntime::new_with_provider(
            "",
            None,
            "test-model",
            None,
            AgentConfig::default(),
            provider,
        )
        .unwrap();
        let mut events = runtime.subscribe();

        runtime.run_accepted("invented-run", "main", 99).await;

        assert!(
            order.lock().unwrap().is_empty(),
            "provider must not be sent"
        );
        let event = events.try_recv().expect("validation error event");
        assert!(matches!(
            event,
            AgentEvent::AgentError { error }
                if error.contains("refusing unvalidated accepted run")
        ));
        assert!(events.try_recv().is_err(), "no start/end events are valid");
    }

    #[tokio::test]
    async fn preparation_finishes_before_provider_started_and_network_send() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let request_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingProvider {
            order: order.clone(),
            message_counts: Arc::new(std::sync::Mutex::new(Vec::new())),
            request_tools: request_tools.clone(),
        });
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = AgentRuntime::new_with_provider(
            "",
            None,
            "effective-model",
            Some(&dir.path().join("session.jsonl")),
            AgentConfig::default(),
            provider,
        )
        .unwrap();
        runtime
            .turn
            .lock()
            .await
            .messages
            .push(AgentMessage::user("test", Vec::new()));

        let prepared_tools = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let manifest_tools = prepared_tools.clone();
        let started_order = order.clone();
        runtime.set_provider_trace_recorder(Some(Arc::new(move |event| {
            let started_order = started_order.clone();
            let manifest_tools = manifest_tools.clone();
            Box::pin(async move {
                match event {
                    crate::provider::ProviderTraceEvent::Started { .. } => {
                        started_order.lock().unwrap().push("started");
                    }
                    crate::provider::ProviderTraceEvent::ContextManifest {
                        context_limit,
                        items,
                        ..
                    } => {
                        assert_eq!(context_limit, Some(128_000));
                        let prepared = manifest_tools.lock().unwrap();
                        let tools = prepared.last().expect("preparer schema before manifest");
                        let tools_json = serde_json::to_string(tools).unwrap();
                        let expected_count = tools.as_array().expect("tool array").len();
                        let expected_label = format!("{expected_count} tools");
                        let expected_digest =
                            format!("{:x}", sha2::Sha256::digest(tools_json.as_bytes()));
                        let tool_items = items
                            .iter()
                            .filter(|item| {
                                item.source == crate::harness::ContextItemSource::ToolSchema
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(tool_items.len(), 1);
                        assert_eq!(
                            tool_items[0].label.as_ref().map(|label| label.as_str()),
                            Some(expected_label.as_str())
                        );
                        assert_eq!(tool_items[0].digest_sha256.as_str(), expected_digest);
                        assert_eq!(
                            tool_items[0].token_estimate,
                            tools_json.len().div_ceil(4) as u32
                        );
                        started_order.lock().unwrap().push("manifest");
                    }
                    _ => {}
                }
                Ok(())
            })
        })));
        let preparer_order = order.clone();
        let preparer_tools = prepared_tools.clone();
        runtime.set_provider_boundary_preparer(Some(Arc::new(
            move |request: ProviderBoundaryRequest| {
                let preparer_order = preparer_order.clone();
                let preparer_tools = preparer_tools.clone();
                Box::pin(async move {
                    assert_eq!(request.model, "effective-model");
                    let tools: serde_json::Value = serde_json::from_str(
                        request
                            .tool_schema_json
                            .as_deref()
                            .expect("shortlisted schema"),
                    )
                    .unwrap();
                    assert!(tools.as_array().is_some_and(|tools| !tools.is_empty()));
                    preparer_tools.lock().unwrap().push(tools);
                    preparer_order.lock().unwrap().push("prepared");
                    Ok(ProviderBoundaryResult {
                        messages: request.messages,
                        context_limit: 128_000,
                        context_limit_is_estimate: true,
                        compaction_generation: 0,
                        provisional_estimated_tokens: None,
                        provider_attempt: None,
                        provider_request_id: None,
                    })
                })
            },
        )));

        runtime.run_turns().await;

        assert_eq!(
            &*order.lock().unwrap(),
            &["prepared", "started", "manifest", "sent"]
        );
        assert_eq!(
            *prepared_tools.lock().unwrap(),
            *request_tools.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn context_manifest_uses_complete_serialized_messages_and_image_cost() {
        let provider = Arc::new(RecordingProvider {
            order: Arc::new(std::sync::Mutex::new(Vec::new())),
            message_counts: Arc::new(std::sync::Mutex::new(Vec::new())),
            request_tools: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let config = AgentConfig::builder().estimated_image_tokens(321).build();
        let messages = vec![
            AgentMessage::Assistant {
                content: Some("calling".into()),
                tool_calls: Some(vec![ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{\"path\":\"README.md\"}".into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: Some("tool_use".into()),
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "read_file".into(),
                content: "contents".into(),
                is_error: false,
                terminate: false,
            },
            AgentMessage::UserWithImages {
                content: "inspect".into(),
                images: vec![crate::types::ImageAttachment {
                    display_name: "screen.png".into(),
                    data_url: "data:image/png;base64,AA==".into(),
                }],
            },
            AgentMessage::Custom {
                custom_type: "thinking".into(),
                payload: serde_json::json!({ "text": "private reasoning" }),
            },
            AgentMessage::Custom {
                custom_type: "extension_note".into(),
                payload: serde_json::json!({ "text": "not provider-visible" }),
            },
            AgentMessage::Custom {
                custom_type: "compaction_summary".into(),
                payload: serde_json::json!({ "summary": "durable checkpoint" }),
            },
        ];
        let mut runtime =
            AgentRuntime::new_with_provider("", None, "test-model", None, config.clone(), provider)
                .unwrap();
        runtime.turn.lock().await.messages = messages.clone();
        let expected = messages.clone();
        runtime.set_provider_trace_recorder(Some(Arc::new(move |event| {
            let expected = expected.clone();
            let config = config.clone();
            Box::pin(async move {
                if let crate::provider::ProviderTraceEvent::ContextManifest {
                    items,
                    total_estimated_tokens,
                    ..
                } = event
                {
                    let message_items = items
                        .iter()
                        .filter(|item| item.source != crate::harness::ContextItemSource::ToolSchema)
                        .collect::<Vec<_>>();
                    assert_eq!(message_items.len(), expected.len());
                    for (item, message) in message_items.iter().zip(&expected) {
                        let normalized = crate::compaction::provider_normalized_message(message);
                        let accounted = normalized.as_ref().unwrap_or(message);
                        let serialized = serde_json::to_vec(accounted).unwrap();
                        assert_eq!(
                            item.digest_sha256.as_str(),
                            format!("{:x}", sha2::Sha256::digest(&serialized))
                        );
                        assert_eq!(
                            item.status,
                            if normalized.is_some() {
                                crate::harness::ContextItemStatus::Active
                            } else {
                                crate::harness::ContextItemStatus::Omitted
                            }
                        );
                        assert_eq!(
                            item.token_estimate as usize,
                            normalized.as_ref().map_or(0, |message| {
                                crate::compaction::estimate_message_tokens(message, &config)
                            })
                        );
                    }
                    assert_eq!(
                        message_items
                            .iter()
                            .filter(|item| {
                                item.status == crate::harness::ContextItemStatus::Omitted
                            })
                            .count(),
                        2
                    );
                    assert!(message_items.last().is_some_and(|item| {
                        item.status == crate::harness::ContextItemStatus::Active
                            && item.token_estimate > 0
                    }));
                    assert_eq!(
                        message_items[1].source,
                        crate::harness::ContextItemSource::ToolResult
                    );
                    assert_eq!(
                        total_estimated_tokens.map(|value| value as usize),
                        Some(items.iter().map(|item| item.token_estimate as usize).sum())
                    );
                }
                Ok(())
            })
        })));

        runtime.run_turns().await;
    }

    #[tokio::test]
    async fn context_manifest_persistence_failure_blocks_network_send() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingProvider {
            order: order.clone(),
            message_counts: Arc::new(std::sync::Mutex::new(Vec::new())),
            request_tools: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let mut runtime = AgentRuntime::new_with_provider(
            "",
            None,
            "test-model",
            None,
            AgentConfig::default(),
            provider,
        )
        .unwrap();
        runtime
            .turn
            .lock()
            .await
            .messages
            .push(AgentMessage::user("test", Vec::new()));
        let trace_order = order.clone();
        runtime.set_provider_trace_recorder(Some(Arc::new(move |event| {
            let trace_order = trace_order.clone();
            Box::pin(async move {
                match event {
                    crate::provider::ProviderTraceEvent::Started { .. } => {
                        trace_order.lock().unwrap().push("started");
                        Ok(())
                    }
                    crate::provider::ProviderTraceEvent::ContextManifest { .. } => {
                        Err("manifest disk full".into())
                    }
                    crate::provider::ProviderTraceEvent::Finished { .. } => {
                        trace_order.lock().unwrap().push("finished");
                        Ok(())
                    }
                    _ => Ok(()),
                }
            })
        })));

        runtime.run_turns().await;

        assert_eq!(&*order.lock().unwrap(), &["started", "finished"]);
    }

    #[tokio::test]
    async fn compaction_persistence_failure_sends_zero_fake_provider_requests() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingProvider {
            order: order.clone(),
            message_counts: Arc::new(std::sync::Mutex::new(Vec::new())),
            request_tools: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let mut runtime = AgentRuntime::new_with_provider(
            "",
            None,
            "test-model",
            None,
            AgentConfig::default(),
            provider,
        )
        .unwrap();
        runtime
            .turn
            .lock()
            .await
            .messages
            .push(AgentMessage::user("test", Vec::new()));
        let started_order = order.clone();
        runtime.set_provider_trace_recorder(Some(Arc::new(move |event| {
            let started_order = started_order.clone();
            Box::pin(async move {
                if matches!(event, crate::provider::ProviderTraceEvent::Started { .. }) {
                    started_order.lock().unwrap().push("started");
                }
                Ok(())
            })
        })));
        // The session layer installs this boundary hook around the durable
        // checkpoint+tail+telemetry commit. Its error must stop the attempt
        // before either provider tracing or ProviderPort::stream_request.
        runtime.set_provider_boundary_preparer(Some(Arc::new(|_| {
            Box::pin(async { Err("compaction persistence failed: disk full".into()) })
        })));

        runtime.run_turns().await;

        assert!(order.lock().unwrap().is_empty());
    }

    struct RateLimitOnceProvider {
        models: Arc<std::sync::Mutex<Vec<String>>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl ProviderPort for RateLimitOnceProvider {
        async fn stream_request(
            &self,
            request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            self.models.lock().unwrap().push(request.model);
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let event = if call == 0 {
                RuntimeStreamEvent::Error("rate limit exceeded".into())
            } else {
                RuntimeStreamEvent::Finished {
                    tool_calls: Vec::new(),
                    usage: RuntimeUsage::default(),
                }
            };
            let _ = events.send(event).await;
        }

        async fn fetch_deferred(
            &self,
            _model: &str,
            _handle_id: &str,
        ) -> Result<DeferredResponse, String> {
            Ok(DeferredResponse::Pending)
        }

        async fn cancel_deferred(&self, _model: &str, _handle_id: &str) -> Result<(), String> {
            Ok(())
        }

        fn provider_kind(&self, _model: &str) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn rate_limit_fallback_is_installed_across_boundary_manifest_and_network() {
        let network_models = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(RateLimitOnceProvider {
            models: network_models.clone(),
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let roles = crate::types::ModelRoles {
            task: Some("primary-model".into()),
            fallback_chain: vec!["primary-model".into(), "fallback-model".into()],
            ..Default::default()
        };
        let config = AgentConfig::builder().model_roles(roles).build();
        let mut runtime =
            AgentRuntime::new_with_provider("", None, "base-model", None, config, provider)
                .unwrap();
        runtime
            .turn
            .lock()
            .await
            .messages
            .push(AgentMessage::user("unchanged request", Vec::new()));

        let prepared_models = Arc::new(std::sync::Mutex::new(Vec::new()));
        let preparer_models = prepared_models.clone();
        runtime.set_provider_boundary_preparer(Some(Arc::new(move |request| {
            let preparer_models = preparer_models.clone();
            Box::pin(async move {
                preparer_models.lock().unwrap().push(request.model.clone());
                let budget =
                    crate::model_metadata::context_budget(&request.model, &AgentConfig::default());
                Ok(ProviderBoundaryResult {
                    messages: request.messages,
                    context_limit: budget.limit,
                    context_limit_is_estimate: budget.limit_is_estimate,
                    compaction_generation: 0,
                    provisional_estimated_tokens: None,
                    provider_attempt: None,
                    provider_request_id: None,
                })
            })
        })));
        let started_models = Arc::new(std::sync::Mutex::new(Vec::new()));
        let manifest_models = Arc::new(std::sync::Mutex::new(Vec::new()));
        let started_capture = started_models.clone();
        let manifest_capture = manifest_models.clone();
        runtime.set_provider_trace_recorder(Some(Arc::new(move |event| {
            let started_capture = started_capture.clone();
            let manifest_capture = manifest_capture.clone();
            Box::pin(async move {
                match event {
                    crate::provider::ProviderTraceEvent::Started { model, .. } => {
                        started_capture.lock().unwrap().push(model);
                    }
                    crate::provider::ProviderTraceEvent::ContextManifest { model, .. } => {
                        manifest_capture.lock().unwrap().push(model);
                    }
                    _ => {}
                }
                Ok(())
            })
        })));

        runtime.run_turns().await;

        let expected = vec!["primary-model".to_string(), "fallback-model".to_string()];
        assert_eq!(*prepared_models.lock().unwrap(), expected);
        assert_eq!(*started_models.lock().unwrap(), expected);
        assert_eq!(*manifest_models.lock().unwrap(), expected);
        assert_eq!(*network_models.lock().unwrap(), expected);
        assert_eq!(
            runtime.turn.lock().await.messages.len(),
            1,
            "no routing reminder"
        );
    }
    struct OverflowOnceProvider {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl ProviderPort for OverflowOnceProvider {
        async fn stream_request(
            &self,
            _request: RuntimeRequest,
            events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
        ) {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let event = match call {
                0 => RuntimeStreamEvent::Error("maximum context length exceeded".into()),
                1 => RuntimeStreamEvent::Finished {
                    tool_calls: vec![ToolCall {
                        id: "call-after-overflow".into(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "missing_test_tool".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    }],
                    usage: RuntimeUsage::default(),
                },
                _ => RuntimeStreamEvent::Finished {
                    tool_calls: Vec::new(),
                    usage: RuntimeUsage::default(),
                },
            };
            let _ = events.send(event).await;
        }

        async fn fetch_deferred(
            &self,
            _model: &str,
            _handle_id: &str,
        ) -> Result<DeferredResponse, String> {
            Ok(DeferredResponse::Pending)
        }

        async fn cancel_deferred(&self, _model: &str, _handle_id: &str) -> Result<(), String> {
            Ok(())
        }

        fn provider_kind(&self, _model: &str) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn overflow_success_with_tool_continuation_marks_only_immediate_retry() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = Arc::new(OverflowOnceProvider {
            calls: calls.clone(),
        });
        let mut runtime = AgentRuntime::new_with_provider(
            "",
            None,
            "test-model",
            None,
            AgentConfig::default(),
            provider,
        )
        .unwrap();
        runtime
            .turn
            .lock()
            .await
            .messages
            .push(AgentMessage::user("test", Vec::new()));
        let recovery_values = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_values = recovery_values.clone();
        runtime.set_provider_boundary_preparer(Some(Arc::new(move |request| {
            let observed_values = observed_values.clone();
            Box::pin(async move {
                observed_values
                    .lock()
                    .unwrap()
                    .push(request.overflow_recovery);
                Ok(ProviderBoundaryResult {
                    messages: request.messages,
                    context_limit: 128_000,
                    context_limit_is_estimate: false,
                    compaction_generation: 0,
                    provisional_estimated_tokens: None,
                    provider_attempt: None,
                    provider_request_id: None,
                })
            })
        })));

        runtime.run_turns().await;

        assert_eq!(&*recovery_values.lock().unwrap(), &[false, true, false]);
        assert_eq!(
            recovery_values
                .lock()
                .unwrap()
                .iter()
                .filter(|value| **value)
                .count(),
            1
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_durable_runtime_keeps_direct_compaction() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let message_counts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingProvider {
            order,
            message_counts: message_counts.clone(),
            request_tools: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let config = AgentConfig::builder()
            .auto_compaction_threshold_tokens(1)
            .auto_compaction_keep_recent_tokens(16)
            .build();
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = AgentRuntime::new_with_provider(
            "",
            None,
            "test-model",
            Some(&dir.path().join("session.jsonl")),
            config,
            provider,
        )
        .unwrap();
        let original_count = 8;
        runtime.turn.lock().await.messages = (0..original_count)
            .map(|index| {
                AgentMessage::user(format!("message {index} {}", "x".repeat(100)), Vec::new())
            })
            .collect();

        runtime.run_turns().await;

        assert!(message_counts.lock().unwrap()[0] < original_count);
    }

    #[tokio::test]
    async fn terminal_queue_persistence_failure_retains_steering_and_follow_up() {
        for steering in [true, false] {
            let order = Arc::new(std::sync::Mutex::new(Vec::new()));
            let provider = Arc::new(RecordingProvider {
                order: order.clone(),
                message_counts: Arc::new(std::sync::Mutex::new(Vec::new())),
                request_tools: Arc::new(std::sync::Mutex::new(Vec::new())),
            });
            let mut runtime = AgentRuntime::new_with_provider(
                "",
                None,
                "test-model",
                None,
                AgentConfig::default(),
                provider,
            )
            .unwrap();
            runtime.turn.lock().await.messages = vec![AgentMessage::user("initial", Vec::new())];
            let queued = AgentMessage::user(
                if steering {
                    "queued steer"
                } else {
                    "queued follow-up"
                },
                Vec::new(),
            );
            if steering {
                runtime.steering_queue.push(queued.clone());
            } else {
                runtime.follow_up_queue.push(queued.clone());
            }
            runtime.set_message_recorder(Some(Arc::new(move |message| {
                let queued = queued.clone();
                Box::pin(async move {
                    if message == queued {
                        Err("injected queue persistence failure".into())
                    } else {
                        Ok(())
                    }
                })
            })));

            runtime.run_turns().await;

            assert_eq!(order.lock().unwrap().len(), usize::from(!steering));
            if steering {
                assert_eq!(runtime.steering_queue.len(), 1);
            } else {
                assert_eq!(runtime.follow_up_queue.len(), 1);
            }
        }
    }
}
