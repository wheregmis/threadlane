//! Turn loop driver for [`UnifiedAgent`].
//!
//! Encapsulates streaming, auto-compaction, journal
//! recording, tool execution, and queue draining for an active turn sequence.

use threadlane_compaction::{
    compact_messages_to_token_budget, estimate_message_tokens_serialized,
    estimate_request_tokens, is_context_overflow_error, provider_normalized_message,
    serialized_message, should_auto_compact, CompactionParams,
};
use crate::config::AgentConfig;
use threadlane_protocol::AgentEvent;
use crate::harness::{
    ContextItemSource, ContextItemStatus, ContextManifestItem, ErrorCategory, ProviderErrorSummary,
    ProviderOutcome, TraceString,
};
use threadlane_loop::LoopDetector;
use crate::provider::{
    ProviderBoundaryPreparer, ProviderBoundaryRequest, ProviderBoundaryResult, ProviderTraceEvent,
    ProviderTraceRecorder,
};
use crate::tool_dispatcher::ToolDispatcher;
use crate::types::{ToolExecutionMode, TurnState};
use threadlane_protocol::{AgentMessage, AgentToolDefinition, TokenUsage};
use crate::utils::AbortOnDrop;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use threadlane_protocol::{
    ProviderPort, RuntimeRequest, RuntimeStreamEvent as StreamEvent, RuntimeToolCall as ToolCall,
};

use tokio::sync::{broadcast, mpsc, Mutex};

const STREAM_CHECKPOINT_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod project_memory_tests {
    use super::{append_project_memory, AgentConfig, AgentMessage};
    use sha2::{Digest, Sha256};

    #[tokio::test]
    async fn project_memory_respects_request_budget_and_missing_or_corrupt_storage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("parser.rs"), "source").unwrap();
        let args = serde_json::json!({"action":"remember", "key":"parser", "content":"Parser owns storage.",
            "sources":[{"path":"parser.rs", "sha256":format!("{:x}", Sha256::digest(b"source"))}]});
        threadlane_tools::try_execute_tool_in_workspace(
            "manage_memory",
            &args.to_string(),
            dir.path(),
        )
        .unwrap();
        let original = vec![AgentMessage::User {
            content: "Inspect parser".into(),
        }];
        let mut sent = original.clone();
        let config = AgentConfig::default();
        assert_eq!(
            append_project_memory(
                &mut sent,
                Some(dir.path().into()),
                "test",
                &config,
                None,
                Some(100_000)
            )
            .await,
            Some(1)
        );
        assert_eq!(sent.len(), 2);
        // Compaction can leave only continuation prompts plus a checkpoint.
        for prompts in [vec!["Continue"], vec!["Continue", "CONTINUE"], vec![""]] {
            let mut continued = vec![AgentMessage::Custom {
                custom_type: "compaction_summary".into(),
                payload: serde_json::json!({"summary":"Inspect parser"}),
            }];
            continued.extend(prompts.into_iter().map(|content| AgentMessage::User {
                content: content.into(),
            }));
            let position = continued.len();
            assert_eq!(
                append_project_memory(
                    &mut continued,
                    Some(dir.path().into()),
                    "test",
                    &config,
                    None,
                    Some(100_000)
                )
                .await,
                Some(position)
            );
        }
        for schema in [None, Some("large tool schema")] {
            let mut limited = original.clone();
            assert_eq!(
                append_project_memory(
                    &mut limited,
                    Some(dir.path().into()),
                    "test",
                    &config,
                    schema,
                    Some(1)
                )
                .await,
                None
            );
            assert_eq!(limited, original);
        }
        let unrelated = tempfile::tempdir().unwrap();
        let mut missing = original.clone();
        assert_eq!(
            append_project_memory(
                &mut missing,
                Some(unrelated.path().into()),
                "test",
                &config,
                None,
                None
            )
            .await,
            None
        );
        assert_eq!(std::fs::read_dir(unrelated.path()).unwrap().count(), 0);
        assert_eq!(missing, original);
        std::fs::write(dir.path().join(".threadlane/memory.json"), b"corrupt").unwrap();
        let mut corrupt = original.clone();
        assert_eq!(
            append_project_memory(
                &mut corrupt,
                Some(dir.path().into()),
                "test",
                &config,
                None,
                None
            )
            .await,
            None
        );
        assert_eq!(corrupt, original);
        assert_eq!(
            std::fs::read(dir.path().join(".threadlane/memory.json")).unwrap(),
            b"corrupt"
        );
    }
}

/// Optional background data lives only in the outgoing request. It never
/// changes canonical messages, compaction, or the durable transcript.
async fn append_project_memory(
    messages: &mut Vec<AgentMessage>,
    root: Option<std::path::PathBuf>,
    model: &str,
    config: &AgentConfig,
    schema: Option<&str>,
    effective_limit: Option<usize>,
) -> Option<usize> {
    let root = root?;
    // Keep the task discoverable when the latest input is just "continue".
    let mut query = messages
        .iter()
        .rev()
        .filter_map(|message| match message {
            AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                Some(content)
            }
            _ => None,
        })
        .take(3)
        .map(|content| content.chars().take(300).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ");
    if query.trim().is_empty()
        || query
            .split_whitespace()
            .all(|word| word.eq_ignore_ascii_case("continue"))
    {
        if let Some(summary) = messages
            .iter()
            .rev()
            .find_map(threadlane_compaction::compaction_summary_text)
        {
            query.push(' ');
            query.extend(summary.chars().take(1_000));
        }
    }
    let recalled = tokio::task::spawn_blocking(move || {
        threadlane_tools::memory::recall_project_memory(&root, &query)
    })
    .await;
    let content = match recalled {
        Ok(Ok(content)) if !content.is_empty() => content,
        Ok(Ok(_)) => return None,
        error => {
            tracing::warn!(
                ?error,
                "Project memory recall unavailable; continuing without findings"
            );
            return None;
        }
    };
    let budget =
        threadlane_context::context_budget(model, &threadlane_context::BudgetConfig::from(config));
    let ceiling = effective_limit
        .unwrap_or(budget.limit)
        .min(budget.trigger_tokens);
    let position = messages.len();
    messages.push(AgentMessage::User { content });
    if estimate_request_tokens(messages, schema, &CompactionParams::from(config)) >= ceiling {
        messages.pop();
        return None;
    }
    Some(position)
}

async fn persist_messages_with(
    recorder: Option<&crate::provider::AssistantMessageRecorder>,
    messages: &[AgentMessage],
) -> Result<(), String> {
    let Some(recorder) = recorder else {
        return Ok(());
    };
    for message in messages {
        recorder(message.clone()).await?;
    }
    Ok(())
}

fn is_quota_or_rate_limit(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("429")
        || error.contains("rate limit")
        || error.contains("rate_limit")
        || error.contains("quota")
        || error.contains("too many requests")
        || error.contains("resource_exhausted")
}

fn classify_provider_error(error: &str) -> ErrorCategory {
    let error = error.to_ascii_lowercase();
    if error.contains("401")
        || error.contains("authentication")
        || error.contains("invalid api key")
    {
        ErrorCategory::Authentication
    } else if error.contains("403") || error.contains("permission denied") {
        ErrorCategory::Authorization
    } else if is_quota_or_rate_limit(&error) {
        ErrorCategory::RateLimit
    } else if error.contains("timeout") || error.contains("timed out") {
        ErrorCategory::Timeout
    } else if error.contains("invalid request") || error.contains("400") {
        ErrorCategory::InvalidRequest
    } else if error.contains("unavailable") || error.contains("503") {
        ErrorCategory::Unavailable
    } else if error.contains("connection") || error.contains("transport") {
        ErrorCategory::Transport
    } else if error.contains("cancel") || error.contains("abort") {
        ErrorCategory::Cancelled
    } else {
        ErrorCategory::Unknown
    }
}

/// Captured result from one provider stream.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct ProviderStepResult {
    pub(crate) text: String,
    pub(crate) reasoning: String,
    #[allow(dead_code)]
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) usage: TokenUsage,
}

/// Accumulates streaming deltas into a single [`ProviderStepResult`].
#[derive(Default)]
pub struct ProviderStepAccumulator {
    text: String,
    reasoning: String,
    result: Option<ProviderStepResult>,
}

impl ProviderStepAccumulator {
    pub(crate) fn push(
        &mut self,
        event: &StreamEvent,
    ) -> Result<Option<ProviderStepResult>, String> {
        match event {
            StreamEvent::ContentToken(token) => self.text.push_str(token),
            StreamEvent::ReasoningToken(token) => self.reasoning.push_str(token),
            StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallArgsDelta { .. } => {}
            StreamEvent::Finished { tool_calls, usage } => {
                let result = ProviderStepResult {
                    text: self.text.clone(),
                    reasoning: self.reasoning.clone(),
                    tool_calls: tool_calls.clone(),
                    usage: TokenUsage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_read_tokens: usage.cache_read_tokens,
                        cache_write_tokens: usage.cache_write_tokens,
                        total_tokens: usage.total_tokens,
                    },
                };
                self.result = Some(result.clone());
                return Ok(Some(result));
            }
            StreamEvent::Error(error) => return Err(error.clone()),
        }
        Ok(None)
    }

    pub(crate) fn finish(&self) -> Result<ProviderStepResult, String> {
        self.result
            .clone()
            .ok_or_else(|| "provider stream ended without a final response".into())
    }
}

pub(crate) struct TurnDriver<'a> {
    pub(crate) turn: Arc<Mutex<TurnState>>,
    pub(crate) provider_client: Arc<dyn ProviderPort>,
    pub(crate) prompt_cache_key: Option<String>,
    pub(crate) tool_dispatcher: ToolDispatcher,
    pub(crate) config: AgentConfig,
    pub(crate) event_tx: broadcast::Sender<AgentEvent>,
    pub(crate) harness_event_hub: crate::harness::HarnessEventHub,
    pub(crate) provider_trace_recorder: Option<ProviderTraceRecorder>,
    pub(crate) provider_boundary_preparer: Option<ProviderBoundaryPreparer>,
    /// Persists model-visible messages before they may affect another provider
    /// request. Durable runtimes install the canonical session-journal writer.
    pub(crate) message_recorder: Option<crate::provider::AssistantMessageRecorder>,
    pub(crate) steering_queue: &'a mut Vec<AgentMessage>,
    pub(crate) follow_up_queue: &'a mut Vec<AgentMessage>,
}

impl<'a> TurnDriver<'a> {
    fn emit_event(&self, event: AgentEvent) {
        let _ = self.event_tx.send(event.clone());
        self.harness_event_hub.publish_agent_event(event);
    }

    async fn record_provider_trace(&self, event: ProviderTraceEvent) -> Result<(), String> {
        match &self.provider_trace_recorder {
            Some(recorder) => recorder(event).await,
            None => Ok(()),
        }
    }

    async fn persist_messages(&self, messages: &[AgentMessage]) -> Result<(), String> {
        persist_messages_with(self.message_recorder.as_ref(), messages).await
    }

    pub(crate) async fn run_turns(&mut self) -> TokenUsage {
        let mut turn_number = 0;
        let mut total_usage = TokenUsage::default();
        let mut overflow_recovery_attempted = false;
        let mut overflow_recovery_pending = false;
        let mut provider_fallback_attempted = false;
        let mut effective_model_override: Option<String> = None;
        let mut loop_detector = LoopDetector::new(
            self.config.loop_guard_enabled,
            self.config.loop_identical_limit,
            self.config.loop_pingpong_rounds,
            self.config.loop_error_limit,
        );
        // Tool schemas rarely change between attempts; reuse the provider
        // payloads and their serialized JSON while the definition set is
        // unchanged instead of re-rendering them every iteration.
        let mut tools_cache: Option<(
            Vec<AgentToolDefinition>,
            Vec<serde_json::Value>,
            Option<String>,
        )> = None;

        'turns: loop {
            turn_number += 1;

            // Repetition cache is per-turn: identical reads may legitimately
            // recur across turns, but serving turn-1 results in turn-5 would
            // be stale. Mutations within a turn invalidate by version.
            self.tool_dispatcher.clear_repetition_cache();

            // Persist the complete steering batch before removing it from the
            // queue or exposing it to provider context.
            if !self.steering_queue.is_empty() {
                let items = self.steering_queue.clone();
                if let Err(error) = self.persist_messages(&items).await {
                    self.emit_event(AgentEvent::AgentError {
                        error: format!("failed to persist steering before provider work: {error}"),
                    });
                    return total_usage;
                }
                self.steering_queue.clear();
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }

            // Auto-compaction is only safe here for runtimes without a durable
            // journal. Durable coding sessions compact at their harness boundary
            // before starting a run, where the checkpoint branch is committed
            // before another provider request can observe it.
            if self.message_recorder.is_none() {
                let mut turn = self.turn.lock().await;
                if should_auto_compact(&turn.messages, &CompactionParams::from(&self.config)) {
                    turn.messages = compact_messages_to_token_budget(
                        &turn.messages,
                        self.config.auto_compaction_keep_recent_tokens,
                    );
                }
            }

            self.emit_event(AgentEvent::TurnStart { turn_number });

            // --- Provider streaming ---
            let model = {
                let turn = self.turn.lock().await;
                // Keep the user-facing base selection unchanged while installing
                // a concrete route for this and subsequent continuation attempts.
                effective_model_override
                    .clone()
                    .unwrap_or_else(|| turn.model.clone())
            };
            let overflow_recovery = std::mem::take(&mut overflow_recovery_pending);
            let tool_definitions = self.tool_dispatcher.configured_tool_definitions();
            let tool_count = tool_definitions.len();
            let (provider_tools, tool_schema_json) = match tools_cache.take() {
                Some((definitions, tools, schema)) if definitions == tool_definitions => {
                    (tools, schema)
                }
                _ => {
                    let tools = tool_definitions
                        .iter()
                        .map(|tool| tool.to_chat_completions_tool())
                        .collect::<Vec<_>>();
                    let schema = (!tools.is_empty())
                        .then(|| serde_json::to_string(&tools).unwrap_or_default());
                    (tools, schema)
                }
            };
            tools_cache = Some((
                tool_definitions,
                provider_tools.clone(),
                tool_schema_json.clone(),
            ));

            let mut boundary_result: Option<ProviderBoundaryResult> = None;
            if let Some(preparer) = &self.provider_boundary_preparer {
                let messages = self.turn.lock().await.messages.clone();
                let prepared = preparer(ProviderBoundaryRequest {
                    attempt: turn_number as u32,
                    model: model.clone(),
                    messages,
                    tool_schema_json: tool_schema_json.clone(),
                    overflow_recovery,
                })
                .await
                .map_err(|error| format!("context preparation failed: {error}"));
                match prepared {
                    Ok(prepared) => {
                        boundary_result = Some(prepared);
                    }
                    Err(error) => {
                        self.emit_event(AgentEvent::AgentError { error });
                        return total_usage;
                    }
                }
            }

            let provider = self.provider_client.provider_kind(&model).to_string();
            let provider_attempt = boundary_result
                .as_ref()
                .and_then(|prepared| prepared.provider_attempt)
                .unwrap_or(turn_number as u32);
            static PROVIDER_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);
            let request_id = boundary_result
                .as_ref()
                .and_then(|prepared| prepared.provider_request_id.clone())
                .unwrap_or_else(|| {
                    format!(
                        "provider-request-{}",
                        PROVIDER_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
                    )
                });
            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let client = self.provider_client.clone();
            let payload_cache_key = self.prompt_cache_key.clone();
            // Take the prepared request vector rather than cloning it; the
            // canonical write-back clones once instead of copying the whole
            // history a second time per attempt.
            let mut request_messages = match boundary_result.as_mut() {
                Some(prepared) => {
                    let messages = std::mem::take(&mut prepared.messages);
                    self.turn.lock().await.messages = prepared
                        .canonical_messages
                        .as_ref()
                        .unwrap_or(&messages)
                        .clone();
                    messages
                }
                None => self.turn.lock().await.messages.clone(),
            };
            let memory_root = self.turn.lock().await.project_root.clone();
            let memory_position = append_project_memory(
                &mut request_messages,
                memory_root,
                &model,
                &self.config,
                tool_schema_json.as_deref(),
                boundary_result.as_ref().map(|prepared| prepared.context_limit),
            )
            .await;
            let request = {
                let turn = self.turn.lock().await;
                RuntimeRequest {
                    model: model.clone(),
                    messages: serde_json::to_value(&request_messages).unwrap_or_default(),
                    tools: serde_json::Value::Array(provider_tools),
                    prompt_cache_key: payload_cache_key,
                    reasoning_effort: turn.reasoning_effort.as_api_str().map(str::to_owned),
                }
            };

            // One accounting pass over the request: the serialized bytes
            // feed the digest and the token estimate for every manifest
            // item, and their sum is the request-level estimate — no
            // separate normalize + serialize sweep per attempt.
            let (manifest_items, total_estimated_tokens) = {
                let params = CompactionParams::from(&self.config);
                let mut items = Vec::new();
                let mut estimated_tokens = 0usize;
                for (idx, message) in request_messages.iter().enumerate() {
                    let normalized = provider_normalized_message(message);
                    let accounted_message = normalized.as_deref().unwrap_or(message);
                    let serialized = serialized_message(accounted_message);
                    let digest = format!("{:x}", Sha256::digest(&serialized));
                    let estimate = normalized
                        .as_deref()
                        .map(|normalized| {
                            estimate_message_tokens_serialized(normalized, &serialized, &params)
                        })
                        .unwrap_or(0);
                    estimated_tokens = estimated_tokens.saturating_add(estimate);
                    let token_estimate = estimate.min(u32::MAX as usize) as u32;
                    let reduced = boundary_result
                        .as_ref()
                        .and_then(|prepared| prepared.canonical_messages.as_ref())
                        .and_then(|messages| messages.get(idx))
                        .is_some_and(|canonical| canonical != message);
                    let status = if normalized.is_some() && reduced {
                        ContextItemStatus::Truncated
                    } else if normalized.is_some() {
                        ContextItemStatus::Active
                    } else {
                        ContextItemStatus::Omitted
                    };
                    let source = match message {
                        AgentMessage::System { .. } => ContextItemSource::SystemPrompt,
                        AgentMessage::Tool { .. } => ContextItemSource::ToolResult,
                        _ => ContextItemSource::Message,
                    };
                    if let (Ok(role), Ok(digest_sha256)) = (
                        TraceString::new(accounted_message.role_str()),
                        TraceString::new(digest),
                    ) {
                        items.push(ContextManifestItem {
                            position: idx,
                            source,
                            entry_id: None,
                            role,
                            token_estimate,
                            status,
                            digest_sha256,
                            label: if memory_position == Some(idx) {
                                Some(TraceString::new("project memory recall").expect("static label"))
                            } else {
                                reduced.then(|| {
                                    TraceString::new("request-only reduction").expect("static label")
                                })
                            },
                        });
                    }
                }
                if let Some(schema) = tool_schema_json.as_deref() {
                    let digest = format!("{:x}", Sha256::digest(schema.as_bytes()));
                    let token_estimate = schema.len().div_ceil(4).min(u32::MAX as usize) as u32;
                    estimated_tokens = estimated_tokens.saturating_add(schema.len().div_ceil(4));
                    if let (Ok(role), Ok(digest_sha256), Ok(label)) = (
                        TraceString::new("tools"),
                        TraceString::new(digest),
                        TraceString::new(format!("{} tools", tool_count)),
                    ) {
                        items.push(ContextManifestItem {
                            position: items.len(),
                            source: ContextItemSource::ToolSchema,
                            entry_id: None,
                            role,
                            token_estimate,
                            status: ContextItemStatus::Active,
                            digest_sha256,
                            label: Some(label),
                        });
                    }
                }
                (items, estimated_tokens.try_into().ok())
            };
            let (context_limit, context_limit_is_estimate, compaction_generation) = boundary_result
                .as_ref()
                .map_or((None, false, 0), |prepared| {
                    (
                        Some(prepared.context_limit),
                        prepared.context_limit_is_estimate,
                        prepared.compaction_generation,
                    )
                });

            // Canonical durable boundary: preparation, request start, exact
            // context manifest, and only then provider I/O.
            if let Err(error) = self
                .record_provider_trace(ProviderTraceEvent::Started {
                    attempt: provider_attempt,
                    request_id: request_id.clone(),
                    model: model.clone(),
                    provider,
                })
                .await
            {
                self.emit_event(AgentEvent::AgentError {
                    error: format!("failed to persist provider request start: {error}"),
                });
                return total_usage;
            }

            if let Err(error) = self
                .record_provider_trace(ProviderTraceEvent::ContextManifest {
                    attempt: provider_attempt,
                    request_id: request_id.clone(),
                    model: model.clone(),
                    context_limit,
                    context_limit_is_estimate,
                    compaction_generation,
                    total_estimated_tokens,
                    items: manifest_items,
                })
                .await
            {
                let terminal_result = self
                    .record_provider_trace(ProviderTraceEvent::Finished {
                        attempt: provider_attempt,
                        request_id: request_id.clone(),
                        outcome: ProviderOutcome::Failed,
                        error: Some(ProviderErrorSummary {
                            category: ErrorCategory::Protocol,
                            code: TraceString::new("context_manifest_persistence_failed").ok(),
                            retryable: false,
                        }),
                        duration_ms: 0,
                        usage: None,
                    })
                    .await;
                let terminal_detail = terminal_result
                    .err()
                    .map(|terminal| format!("; terminal record also failed: {terminal}"))
                    .unwrap_or_default();
                self.emit_event(AgentEvent::AgentError {
                    error: format!(
                        "failed to persist request context manifest: {error}{terminal_detail}"
                    ),
                });
                return total_usage;
            }
            let request_started_at = Instant::now();
            let mut provider_terminal_recorded = false;

            let _stream_task = AbortOnDrop::new(tokio::spawn(async move {
                client.stream_request(request, stream_tx).await;
            }));

            self.emit_event(AgentEvent::MessageStart {
                role: "assistant".into(),
            });

            let mut current_text = String::new();
            let mut current_reasoning = String::new();
            let mut checkpoint_index = 0u32;
            let mut checkpointed_bytes = 0usize;
            let mut captured_tool_calls: Vec<ToolCall> = Vec::new();
            let mut provider_step = ProviderStepAccumulator::default();

            let mut pending_evt = None;
            while let Some(evt) = match pending_evt.take() {
                Some(evt) => Some(evt),
                None => stream_rx.recv().await,
            } {
                let _ = provider_step.push(&evt);
                match evt {
                    StreamEvent::ContentToken(mut token) => {
                        while let Ok(next_evt) = stream_rx.try_recv() {
                            let _ = provider_step.push(&next_evt);
                            match next_evt {
                                StreamEvent::ContentToken(next_token) => {
                                    token.push_str(&next_token);
                                }
                                other => {
                                    pending_evt = Some(other);
                                    break;
                                }
                            }
                        }
                        current_text.push_str(&token);
                        if current_text.len().saturating_sub(checkpointed_bytes)
                            >= STREAM_CHECKPOINT_BYTES
                        {
                            checkpoint_index = checkpoint_index.saturating_add(1);
                            if let Err(error) = self
                                .record_provider_trace(ProviderTraceEvent::Checkpoint {
                                    attempt: provider_attempt,
                                    request_id: request_id.clone(),
                                    checkpoint_index,
                                    text: current_text.clone(),
                                    reasoning: None,
                                })
                                .await
                            {
                                self.emit_event(AgentEvent::AgentError {
                                    error: format!("failed to persist stream checkpoint: {error}"),
                                });
                                return total_usage;
                            }
                            checkpointed_bytes = current_text.len();
                        }
                        self.emit_event(AgentEvent::MessageUpdate {
                            text_delta: Some(token),
                            reasoning_delta: None,
                            tool_call_name: None,
                        });
                    }
                    StreamEvent::ReasoningToken(mut token) => {
                        while let Ok(next_evt) = stream_rx.try_recv() {
                            let _ = provider_step.push(&next_evt);
                            match next_evt {
                                StreamEvent::ReasoningToken(next_token) => {
                                    token.push_str(&next_token);
                                }
                                other => {
                                    pending_evt = Some(other);
                                    break;
                                }
                            }
                        }
                        current_reasoning.push_str(&token);
                        self.emit_event(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: Some(token),
                            tool_call_name: None,
                        });
                    }
                    StreamEvent::ToolCallStart { name, .. } => {
                        self.emit_event(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: None,
                            tool_call_name: Some(name),
                        });
                    }
                    StreamEvent::ToolCallArgsDelta { .. } => {}
                    StreamEvent::Finished { tool_calls, usage } => {
                        captured_tool_calls = tool_calls;
                        let usage = TokenUsage {
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                            cache_read_tokens: usage.cache_read_tokens,
                            cache_write_tokens: usage.cache_write_tokens,
                            total_tokens: usage.total_tokens,
                        };
                        total_usage.accumulate(&usage);
                        tracing::info!(
                            turn = turn_number,
                            tool_calls = captured_tool_calls.len(),
                            total_tokens = usage.total_tokens,
                            "provider turn finished"
                        );
                        if let Err(error) = self
                            .record_provider_trace(ProviderTraceEvent::Finished {
                                attempt: provider_attempt,
                                request_id: request_id.clone(),
                                outcome: ProviderOutcome::Completed,
                                error: None,
                                duration_ms: request_started_at.elapsed().as_millis() as u64,
                                usage: Some(usage),
                            })
                            .await
                        {
                            self.emit_event(AgentEvent::AgentError {
                                error: format!(
                                    "failed to persist provider request finish: {error}"
                                ),
                            });
                            return total_usage;
                        }
                        provider_terminal_recorded = true;
                        break;
                    }
                    StreamEvent::Error(err) => {
                        tracing::error!(turn = turn_number, error = %err, "provider turn failed");
                        if current_text.len() > checkpointed_bytes {
                            checkpoint_index = checkpoint_index.saturating_add(1);
                            if let Err(error) = self
                                .record_provider_trace(ProviderTraceEvent::Checkpoint {
                                    attempt: provider_attempt,
                                    request_id: request_id.clone(),
                                    checkpoint_index,
                                    text: current_text.clone(),
                                    reasoning: None,
                                })
                                .await
                            {
                                self.emit_event(AgentEvent::AgentError {
                                    error: format!("failed to persist error checkpoint: {error}"),
                                });
                                return total_usage;
                            }
                        }
                        let category = classify_provider_error(&err);
                        let retryable = matches!(
                            category,
                            ErrorCategory::RateLimit
                                | ErrorCategory::Timeout
                                | ErrorCategory::Transport
                                | ErrorCategory::Unavailable
                        );
                        // Store the full provider error text so the trajectory
                        // inspector can show it inline with the request row.
                        let error_code =
                            TraceString::new(err.chars().take(2048).collect::<String>()).ok();
                        if let Err(error) = self
                            .record_provider_trace(ProviderTraceEvent::Finished {
                                attempt: provider_attempt,
                                request_id: request_id.clone(),
                                outcome: ProviderOutcome::Failed,
                                error: Some(ProviderErrorSummary {
                                    category,
                                    code: error_code,
                                    retryable,
                                }),
                                duration_ms: request_started_at.elapsed().as_millis() as u64,
                                usage: None,
                            })
                            .await
                        {
                            self.emit_event(AgentEvent::AgentError {
                                error: format!(
                                    "failed to persist provider request failure: {error}"
                                ),
                            });
                            return total_usage;
                        }
                        if !overflow_recovery_attempted
                            && is_context_overflow_error(&err)
                            && (self.provider_boundary_preparer.is_some()
                                || self.message_recorder.is_none())
                        {
                            if self.provider_boundary_preparer.is_none() {
                                let mut turn = self.turn.lock().await;
                                turn.messages = compact_messages_to_token_budget(
                                    &turn.messages,
                                    self.config.auto_compaction_keep_recent_tokens,
                                );
                            }
                            overflow_recovery_attempted = true;
                            overflow_recovery_pending = true;
                            continue 'turns;
                        }
                        if !provider_fallback_attempted
                            && current_text.is_empty()
                            && is_quota_or_rate_limit(&err)
                        {
                            if let Some(fallback) = self
                                .config
                                .model_roles
                                .fallback_after(&model)
                                .map(str::to_owned)
                            {
                                provider_fallback_attempted = true;
                                effective_model_override = Some(fallback);
                                continue 'turns;
                            }
                        }
                        self.emit_event(AgentEvent::AgentError { error: err });
                        return total_usage;
                    }
                }
            }

            if !provider_terminal_recorded {
                if let Err(error) = self
                    .record_provider_trace(ProviderTraceEvent::Finished {
                        attempt: provider_attempt,
                        request_id: request_id.clone(),
                        outcome: ProviderOutcome::Failed,
                        error: Some(ProviderErrorSummary {
                            category: ErrorCategory::Protocol,
                            code: TraceString::new("stream_closed_without_terminal_event").ok(),
                            retryable: true,
                        }),
                        duration_ms: request_started_at.elapsed().as_millis() as u64,
                        usage: None,
                    })
                    .await
                {
                    self.emit_event(AgentEvent::AgentError {
                        error: format!("failed to persist incomplete provider request: {error}"),
                    });
                    return total_usage;
                }
            }

            if current_text.trim().is_empty() && captured_tool_calls.is_empty() {
                let error = match provider_step.finish() {
                    Ok(_) => {
                        let phase = if turn_number > 1 {
                            " after tool execution"
                        } else {
                            ""
                        };
                        format!("Provider returned an empty response{phase} (turn {turn_number})")
                    }
                    Err(error) => format!(
                        "Provider stream ended without a final response (turn {turn_number}): {error}"
                    ),
                };
                tracing::warn!(turn = turn_number, error = %error, "provider returned no usable response");
                self.emit_event(AgentEvent::AgentError { error });
                return total_usage;
            }

            // Record assistant message in turn state.
            let assistant_msg = AgentMessage::Assistant {
                content: if current_text.is_empty() {
                    None
                } else {
                    Some(current_text.clone())
                },
                tool_calls: if captured_tool_calls.is_empty() {
                    None
                } else {
                    Some(captured_tool_calls.clone())
                },
                stop_reason: None,
                deferred_handle: None,
            };

            let mut step_messages = Vec::new();
            if !current_reasoning.trim().is_empty() {
                let thinking = AgentMessage::Custom {
                    custom_type: "thinking".into(),
                    payload: serde_json::json!({ "text": current_reasoning }),
                };
                step_messages.push(thinking);
            }

            step_messages.push(assistant_msg.clone());

            // Persist the typed assistant transition before exposing it to the
            // next continuation. The transient turn copy is updated only
            // after the canonical commit succeeds.
            if let Err(error) = self.persist_messages(&step_messages).await {
                self.emit_event(AgentEvent::AgentError {
                    error: format!("failed to persist assistant step before continuation: {error}"),
                });
                return total_usage;
            }
            if let Err(error) = self
                .record_provider_trace(ProviderTraceEvent::AssistantReady {
                    attempt: provider_attempt,
                    request_id: request_id.clone(),
                    reasoning: (!current_reasoning.trim().is_empty())
                        .then(|| current_reasoning.clone()),
                    message: assistant_msg.clone(),
                })
                .await
            {
                self.emit_event(AgentEvent::AgentError {
                    error: format!("failed to persist provider assistant result: {error}"),
                });
                return total_usage;
            }
            self.turn
                .lock()
                .await
                .messages
                .extend(step_messages.iter().cloned());

            self.emit_event(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
                self.emit_event(AgentEvent::TurnEnd {
                    turn_number,
                    tool_results: Vec::new(),
                });

                if !self.steering_queue.is_empty() {
                    let items = self.steering_queue.clone();
                    if let Err(error) = self.persist_messages(&items).await {
                        self.emit_event(AgentEvent::AgentError {
                            error: format!("failed to persist steering before retry: {error}"),
                        });
                        return total_usage;
                    }
                    self.steering_queue.clear();
                    self.turn.lock().await.messages.extend(items);
                    continue;
                }

                if !self.follow_up_queue.is_empty() {
                    let items = self.follow_up_queue.clone();
                    if let Err(error) = self.persist_messages(&items).await {
                        self.emit_event(AgentEvent::AgentError {
                            error: format!(
                                "failed to persist follow-up before provider work: {error}"
                            ),
                        });
                        return total_usage;
                    }
                    self.follow_up_queue.clear();
                    self.turn.lock().await.messages.extend(items);
                    continue;
                }
                break;
            }

            // Persist a bounded handoff checkpoint before external tool work.
            if current_text.len() > checkpointed_bytes {
                checkpoint_index = checkpoint_index.saturating_add(1);
                if let Err(error) = self
                    .record_provider_trace(ProviderTraceEvent::Checkpoint {
                        attempt: provider_attempt,
                        request_id: request_id.clone(),
                        checkpoint_index,
                        text: current_text.clone(),
                        reasoning: None,
                    })
                    .await
                {
                    self.emit_event(AgentEvent::AgentError {
                        error: format!("failed to persist pre-tool checkpoint: {error}"),
                    });
                    return total_usage;
                }
            }

            // Execute tools.
            let mut dispatcher = self.tool_dispatcher.clone();
            dispatcher.tool_execution_mode = ToolExecutionMode::Parallel;

            let tool_results = match dispatcher.execute_tools(&captured_tool_calls).await {
                Ok(results) => results,
                Err(error) => {
                    self.emit_event(AgentEvent::AgentError { error: error.to_string() });
                    return total_usage;
                }
            };

            // Persist tool results before they can affect the continuation
            // request. Tool lifecycle recorders may enrich the same durable
            // operation, while this recorder guarantees model visibility.
            let tool_messages = tool_results
                .iter()
                .map(|result| AgentMessage::Tool {
                    tool_call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    terminate: result.terminate,
                    images: result.images.clone(),
                })
                .collect::<Vec<_>>();
            if let Err(error) = self.persist_messages(&tool_messages).await {
                self.emit_event(AgentEvent::AgentError {
                    error: format!("failed to persist tool results before continuation: {error}"),
                });
                return total_usage;
            }
            self.turn.lock().await.messages.extend(tool_messages);

            self.emit_event(AgentEvent::TurnEnd {
                turn_number,
                tool_results: tool_results.clone(),
            });

            // Loop circuit breaker: feed executed calls in order and stop
            // terminally on repetition tripwires. Checked before the normal
            // terminate break so a looping run reports the loop, not success.
            let mut trip = None;
            for (call, result) in captured_tool_calls.iter().zip(tool_results.iter()) {
                if let Some(tripped) = loop_detector.observe(
                    &call.function.name,
                    &call.function.arguments,
                    &result.content,
                    result.is_error,
                ) {
                    trip = Some(tripped);
                    break;
                }
            }
            if let Some(trip) = trip {
                let message = trip.message();
                let assistant_msg = AgentMessage::Assistant {
                    content: Some(message.clone()),
                    tool_calls: None,
                    stop_reason: Some("loop_guard".into()),
                    deferred_handle: None,
                };
                if let Err(error) = self.persist_messages(&[assistant_msg.clone()]).await {
                    self.emit_event(AgentEvent::AgentError {
                        error: format!("failed to persist loop-guard message: {error}"),
                    });
                    return total_usage;
                }
                self.turn.lock().await.messages.push(assistant_msg);
                self.emit_event(AgentEvent::StreamRuleTriggered {
                    rule_id: "loop-guard".into(),
                    rule_name: trip.rule_name().into(),
                    matched_text: trip.message(),
                    reminder: message,
                });
                break;
            }

            if tool_results.iter().any(|r| r.terminate) {
                break;
            }
        }
        total_usage
    }
}
