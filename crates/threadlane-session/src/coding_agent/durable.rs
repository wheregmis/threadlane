use super::cancellation::{recover_v2_subagent_records, AgentRunTask};
use super::capabilities::dispatch_hook_requests;
use super::harness::{
    CodingSessionHarness, InterruptedSubagentRecoveryState, SubagentLaneIdentity,
};
use super::runtime::CodingAgent;
use super::subagents::{
    run_subagent_task, SubagentLaneStatus, SubagentRunContext, NEXT_SUBAGENT_UI_RUN_ID,
};
use crate::agents::AgentDefinition;
use crate::commands::{execute_slash_command, parse_slash_command};
use log::warn;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use threadlane_runtime::harness::{
    HookContext, HookKind, JsonlStore, OperationOutcome, PromptSnapshot, Record as HarnessRecord,
    Reducer, SessionStore,
};
use threadlane_runtime::{AgentEvent, AgentMessage, AgentToolResult, SubagentRecoveryStatus};
use tokio::sync::broadcast;

pub(crate) const MAX_PERSISTED_SYSTEM_PROMPT_BYTES: usize = 256 * 1024;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn should_complete_prewalk(tool_name: &str, is_error: bool) -> bool {
    !is_error && tool_name == super::capabilities::PREWALK_HANDOFF_TOOL_NAME
}

#[cfg(test)]
mod prewalk_tests {
    use super::should_complete_prewalk;

    #[test]
    fn only_a_successful_explicit_signal_completes_prewalk() {
        assert!(!should_complete_prewalk("write_file", false));
        assert!(!should_complete_prewalk("edit_file_hashline", false));
        assert!(should_complete_prewalk(
            super::super::capabilities::PREWALK_HANDOFF_TOOL_NAME,
            false
        ));
        assert!(!should_complete_prewalk(
            super::super::capabilities::PREWALK_HANDOFF_TOOL_NAME,
            true
        ));
    }
}

pub(crate) fn durable_prompt_snapshot(content: &str) -> PromptSnapshot {
    let sha256 = threadlane_runtime::harness::TraceString::new(sha256_hex(content.as_bytes()))
        .expect("sha256 digest is bounded");
    let explicitly_redacted = std::env::var("THREADLANE_REDACT_SYSTEM_PROMPTS")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    if explicitly_redacted || content.len() > MAX_PERSISTED_SYSTEM_PROMPT_BYTES {
        PromptSnapshot::Redacted {
            sha256,
            byte_len: content.len(),
            reason: threadlane_runtime::harness::TraceString::new(if explicitly_redacted {
                "configured_redaction"
            } else {
                "size_limit"
            })
            .expect("redaction reason is bounded"),
        }
    } else {
        PromptSnapshot::Full {
            content: threadlane_runtime::harness::BoundedPromptText::new(content)
                .expect("system prompt is within byte limit"),
            sha256,
        }
    }
}

pub(crate) fn is_retryable_generation_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "timeout",
        "timed out",
        "connection reset",
        "connection refused",
        "temporarily unavailable",
        "rate limit",
        "status 429",
        "status 502",
        "status 503",
        "status 504",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}

pub(crate) fn generation_event_drain_error(
    error: broadcast::error::TryRecvError,
) -> Option<&'static str> {
    match error {
        broadcast::error::TryRecvError::Lagged(_) => None,
        broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed => {
            Some("generation ended without a durable AgentEnd event")
        }
    }
}

pub(crate) fn requires_harness_compaction_reset(
    durable_messages: &[AgentMessage],
    state_messages: &[AgentMessage],
) -> bool {
    state_messages
        .iter()
        .any(|message| threadlane_runtime::compaction_summary_text(message).is_some())
        && !state_messages.starts_with(durable_messages)
}

pub(crate) fn compaction_retained_tail(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    let Some(summary_index) = messages
        .iter()
        .rposition(|message| threadlane_runtime::compaction_summary_text(message).is_some())
    else {
        return Vec::new();
    };
    messages
        .iter()
        .skip(summary_index + 1)
        .filter(|message| !matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect()
}

impl CodingAgent {
    fn install_run_trace_recorders(
        &mut self,
        path: PathBuf,
        run_id: String,
    ) -> Result<(), String> {
        let trace_harness = Arc::new(tokio::sync::Mutex::new(CodingSessionHarness::open(&path)?));
        let provider_harness = trace_harness.clone();
        let provider_run_id = run_id.clone();
        self.agent
            .set_provider_trace_recorder(Some(Arc::new(move |event| {
                let harness = provider_harness.clone();
                let run_id = provider_run_id.clone();
                Box::pin(async move { harness.lock().await.record_provider_trace(&run_id, event) })
            })));
        let message_harness = trace_harness.clone();
        let message_run_id = run_id.clone();
        let message_work_dir = self.work_dir.clone();
        let boundary_harness = trace_harness.clone();
        let boundary_run_id = run_id.clone();
        let boundary_config = self.agent.config().clone();
        self.agent
            .set_provider_boundary_preparer(Some(Arc::new(move |request| {
                let harness = boundary_harness.clone();
                let run_id = boundary_run_id.clone();
                let config = boundary_config.clone();
                Box::pin(async move {
                    let mut harness = harness.lock().await;
                    harness.prepare_provider_boundary(&run_id, request, &config)
                })
            })));
        self.agent
            .set_message_recorder(Some(Arc::new(move |message| {
                let harness = message_harness.clone();
                let run_id = message_run_id.clone();
                let work_dir = message_work_dir.clone();
                Box::pin(async move {
                    let mut harness = harness.lock().await;
                    let output_chars = match &message {
                        AgentMessage::Tool {
                            content,
                            name,
                            is_error: false,
                            ..
                        } if name == "read_file" => Some(content.chars().count()),
                        _ => None,
                    };
                    let tool_call_id = match &message {
                        AgentMessage::Tool {
                            tool_call_id,
                            name,
                            is_error: false,
                            ..
                        } if name == "read_file" => Some(tool_call_id.clone()),
                        _ => None,
                    };
                    let entry_id = harness.append_message(message)?;
                    if let (Some(tool_call_id), Some(output_chars)) = (tool_call_id, output_chars) {
                        if let Err(error) = harness.index_read_snapshot(
                            &run_id,
                            &work_dir,
                            &tool_call_id,
                            &entry_id,
                            output_chars,
                        ) {
                            warn!("failed to index read_file context snapshot: {error}");
                        }
                    }
                    Ok(())
                })
            })));
        let intent_harness = trace_harness.clone();
        let intent_run_id = run_id.clone();
        self.agent.tool_dispatcher.tool_intent_recorder =
            Some(Arc::new(move |id, name, arguments| {
                let harness = intent_harness.clone();
                let run_id = intent_run_id.clone();
                let id = id.to_string();
                let name = name.to_string();
                let arguments = arguments.to_string();
                Box::pin(async move {
                    let effective_args = serde_json::from_str(&arguments)
                        .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()));
                    harness
                        .lock()
                        .await
                        .append_tool_intent_after_hook(&run_id, &id, &name, effective_args)
                        .await
                })
            }));
        let tool_harness = trace_harness.clone();
        let tool_run_id = run_id.clone();
        self.agent.tool_dispatcher.tool_execution_trace_recorder = Some(Arc::new(move |event| {
            let harness = tool_harness.clone();
            let run_id = tool_run_id.clone();
            Box::pin(async move {
                harness
                    .lock()
                    .await
                    .record_tool_execution(&run_id, event)
                    .await
            })
        }));
        let completion_harness = trace_harness.clone();
        let completion_run_id = run_id.clone();
        let prewalk_arc = self.prewalk.clone();
        let event_tx = self.agent.event_tx.clone();
        let turn_arc = self.agent.turn.clone();
        self.agent.tool_dispatcher.tool_completion_recorder = Some(Arc::new(move |result| {
            let harness = completion_harness.clone();
            let run_id = completion_run_id.clone();
            let result = result.clone();
            let prewalk = prewalk_arc.clone();
            let event_tx = event_tx.clone();
            let turn_arc = turn_arc.clone();
            Box::pin(async move {
                if should_complete_prewalk(&result.name, result.is_error) {
                    let state = prewalk.lock().unwrap().take();
                    if let Some(state) = state {
                        let handoff_ms = state.started_at.elapsed().as_millis();
                        let target_model = state.target_model;
                        let target_effort = state.target_reasoning;
                        let mut turn = turn_arc.lock().await;
                        turn.model = target_model.clone();
                        if let Some(effort) = target_effort {
                            turn.reasoning_effort = effort;
                        }
                        if let Some(pos) = turn
                            .system_prompt
                            .find(crate::orchestrator::ARCHITECT_PROTOCOL_HEADER)
                        {
                            turn.system_prompt.truncate(pos);
                            turn.system_prompt = turn.system_prompt.trim_end().to_string();
                        }
                        drop(turn);
                        let effort_info = target_effort
                            .map(|e| format!(" with reasoning effort `{}`", e.label()))
                            .unwrap_or_default();
                        let _ = event_tx.send(threadlane_runtime::AgentEvent::PrewalkCompleted {
                            model: target_model.clone(),
                            message: format!(
                                "Prewalk complete: foundational change verified. Switched model to `{target_model}`{effort_info}."
                            ),
                        });
                        log::info!(
                            "orchestrator handoff_ms={handoff_ms} handoff_model={target_model} success=true"
                        );
                    }
                }
                harness.lock().await.record_tool_result(&run_id, &result)
            })
        }));
        let permission_harness = trace_harness;
        self.permission_handle
            .set_trace_recorder(Some(Arc::new(move |event| {
                let harness = permission_harness.clone();
                let run_id = run_id.clone();
                Box::pin(async move {
                    harness
                        .lock()
                        .await
                        .record_permission_trace(Some(&run_id), event)
                })
            })));
        Ok(())
    }

    pub(crate) async fn execute_accepted_run(
        &mut self,
        accepted: &threadlane_runtime::harness::AcceptedRun,
    ) -> Result<(), String> {
        if accepted.lane != "main"
            || accepted.accepted_through_seq == 0
            || accepted.prompt_entry_id.is_empty()
            || accepted.assistant_entry_id.is_empty()
        {
            return Err("invalid accepted run proof".into());
        }
        if let Some(harness) = self.harness.as_ref() {
            harness.validate_accepted_run(accepted)?;
        }
        self.sync_turn_from_model_context().await?;
        let last_prompt = {
            let turn = self.agent.turn.lock().await;
            turn.messages.iter().rev().find_map(|m| match m {
                AgentMessage::User { content } => Some(content.clone()),
                _ => None,
            })
        };
        if let Some(ref prompt) = last_prompt {
            let trimmed = prompt.trim();
            if let Some(command_input) = trimmed.strip_prefix('/') {
                let mut parts = command_input.split_whitespace();
                let cmd_name = parts.next().unwrap_or("");
                let cmd_args = parts.collect::<Vec<&str>>().join(" ");
                if cmd_name == "subagent" {
                    let task_prompt = cmd_args.trim();
                    if task_prompt.is_empty() {
                        return Err("Usage: /subagent <task description>".into());
                    }
                    let task = AgentRunTask {
                        agent: "worker".to_string(),
                        task: task_prompt.to_string(),
                        instructions: None,
                        tools: None,
                        model: None,
                        context_refs: Vec::new(),
                    };
                    let parent_leaf =
                        self.prompt_parent_leaf(AgentMessage::user(prompt, Vec::new()), true);
                    *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                    let result = match (self.agent_runner)(vec![task], false, None).await {
                        Ok(result) => result,
                        Err(err) => {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            return Err(format!("Subagent Error: {err}"));
                        }
                    };
                    let output = result["output"].as_str().unwrap_or_default().to_string();
                    if let Err(error) = self.commit_completed_subagent_lanes() {
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        return Err(format!("Subagent Sync Error: {error}"));
                    }
                    *self.dispatch_parent_leaf.lock().unwrap() = None;

                    if let Some(harness) = self.harness.as_mut() {
                        let _ = harness.append_message_to_lane(
                            "main",
                            &accepted.run_id,
                            AgentMessage::Assistant {
                                content: Some(output),
                                tool_calls: None,
                                stop_reason: None,
                                deferred_handle: None,
                            },
                        );
                    }
                    return Ok(());
                }
            }
            if let Some(cmd_action) = parse_slash_command(prompt) {
                let output = execute_slash_command(cmd_action, &mut self.agent).await;
                if let Some(harness) = self.harness.as_mut() {
                    let _ = harness.append_message_to_lane(
                        "main",
                        &accepted.run_id,
                        AgentMessage::Assistant {
                            content: Some(output),
                            tool_calls: None,
                            stop_reason: None,
                            deferred_handle: None,
                        },
                    );
                }
                return Ok(());
            }
        }
        self.harness_journal_error = None;
        self.agent
            .run_accepted(
                &accepted.run_id,
                &accepted.lane,
                accepted.accepted_through_seq,
            )
            .await;
        self.sync_harness_and_dispatch_assistant_hooks().await;
        if let Some(error) = self.harness_journal_error.take() {
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn begin_harness_run(
        &mut self,
        prompt: AgentMessage,
    ) -> Result<Option<threadlane_runtime::harness::AcceptedRun>, String> {
        if let Some(run_id) = self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())?
            .clone()
        {
            return Err(format!(
                "run {run_id} is already active; prompt acceptance cannot be repeated"
            ));
        }
        let model = self.agent.model().to_string();
        // The router has no ACP branch and would label an ACP run as an
        // OpenAI one, which makes the trajectory misreport what actually ran.
        let provider = if crate::acp_bridge::is_acp_model(&model) {
            "acp".to_string()
        } else {
            self.agent
                .provider_client()
                .provider_kind(&model)
                .to_string()
        };
        let tool_definitions = self.agent.configured_tool_definitions();
        let tool_schema = serde_json::to_vec(&tool_definitions)
            .map_err(|error| format!("failed to serialize resolved tool schema: {error}"))?;
        let enabled_tool_names = tool_definitions
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let mut capabilities = enabled_tool_names
            .iter()
            .map(|name| format!("tool:{name}"))
            .collect::<Vec<_>>();
        capabilities.extend(
            self.skills
                .list_skills()
                .into_iter()
                .filter(|skill| skill.enabled && skill.is_valid)
                .map(|skill| format!("skill:{}", skill.id)),
        );
        capabilities.extend(
            self.wasi_extensions
                .extension_manifests()
                .into_iter()
                .map(|extension| format!("extension:{}", extension.name)),
        );
        capabilities.push(format!("tool_policy:{:?}", *self.tool_policy.lock().await));
        capabilities.sort();
        capabilities.dedup();
        let capability_sha256 = sha256_hex(
            capabilities
                .join(
                    "
",
                )
                .as_bytes(),
        );
        let prompt_template_ids = self
            .prompt_templates
            .as_ref()
            .map(|templates| {
                templates
                    .iter()
                    .map(|template| template.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let system_prompt = durable_prompt_snapshot(&self.agent.system_prompt());
        let context_window_limit = Some(
            threadlane_runtime::model_metadata::context_budget(&model, self.agent.config()).limit,
        );
        let work_dir = self.work_dir.to_string_lossy().into_owned();
        let Some(journal) = self.harness.as_mut() else {
            return Ok(None);
        };
        let run_id = journal.unique_run_id("foreground")?;
        let accepted = journal.begin_run(&run_id, prompt)?;
        journal.capture_run_context(
            &run_id,
            "main",
            model,
            provider,
            self.agent.reasoning_effort(),
            self.agent.prompt_cache_enabled(),
            work_dir,
            system_prompt,
            sha256_hex(&tool_schema),
            enabled_tool_names,
            capabilities,
            Some(capability_sha256),
            prompt_template_ids,
            None,
            context_window_limit,
        )?;
        let context = HookContext {
            session_id: journal.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.clone()),
            resume_data: None,
            tool_call_id: None,
            tool_name: None,
            tool_arguments: None,
            tool_result_content: None,
            tool_result_is_error: None,
        };
        for failure in journal
            .store
            .hooks()
            .run(HookKind::BeforeRun, &context)
            .await
        {
            warn!("before-run hook {} failed: {}", failure.id, failure.message);
        }
        *self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())? = Some(run_id.clone());
        if let Some(path) = self.session_file.clone() {
            self.install_run_trace_recorders(path, run_id.clone())?;
        }
        Ok(Some(accepted))
    }

    pub(crate) fn adopt_harness_run(
        &mut self,
        accepted: &threadlane_runtime::harness::AcceptedRun,
    ) -> Result<(), String> {
        let run_id = accepted.run_id.as_str();
        if accepted.lane != "main" || accepted.accepted_through_seq == 0 {
            return Err("invalid accepted run proof".into());
        }
        let Some(journal) = self.harness.as_mut() else {
            return Ok(());
        };
        journal.ensure_fresh()?;
        journal
            .store
            .validate_accepted_run(accepted)
            .map_err(|error| error.to_string())?;
        let state = Reducer::reduce(&journal.store).map_err(|error| error.to_string())?;
        let Some(open_run) = state
            .lane("main")
            .and_then(|lane| lane.open_operation.as_deref())
        else {
            return Err(format!("harness operation {run_id} is not open on main"));
        };
        if open_run != run_id {
            return Err(format!("harness operation {run_id} is not open on main"));
        }
        let has_context = journal.store.records().iter().any(|record| {
            matches!(
                record,
                HarnessRecord::RunContextCaptured {
                    run_id: captured_run_id,
                    ..
                } if captured_run_id == run_id
            )
        });
        if !has_context {
            let turn = self
                .agent
                .turn
                .try_lock()
                .map_err(|_| "adopted run context is currently locked".to_string())?;
            let model = turn.model.clone();
            let provider = self
                .agent
                .provider_client()
                .provider_kind(&model)
                .to_string();
            let tool_definitions = self.agent.configured_tool_definitions();
            let tool_schema = serde_json::to_vec(&tool_definitions)
                .map_err(|error| format!("failed to serialize resolved tool schema: {error}"))?;
            let enabled_tool_names = tool_definitions
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            let mut capabilities = enabled_tool_names
                .iter()
                .map(|name| format!("tool:{name}"))
                .collect::<Vec<_>>();
            capabilities.extend(
                self.skills
                    .list_skills()
                    .into_iter()
                    .filter(|skill| skill.enabled && skill.is_valid)
                    .map(|skill| format!("skill:{}", skill.id)),
            );
            capabilities.extend(
                self.wasi_extensions
                    .extension_manifests()
                    .into_iter()
                    .map(|extension| format!("extension:{}", extension.name)),
            );
            if let Ok(policy) = self.tool_policy.try_lock() {
                capabilities.push(format!("tool_policy:{policy:?}"));
            }
            capabilities.sort();
            capabilities.dedup();
            let capability_sha256 = sha256_hex(
                capabilities
                    .join(
                        "
",
                    )
                    .as_bytes(),
            );
            let prompt_template_ids = self
                .prompt_templates
                .as_ref()
                .map(|templates| {
                    templates
                        .iter()
                        .map(|template| template.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            let context_window_limit = Some(
                threadlane_runtime::model_metadata::context_budget(&model, self.agent.config())
                    .limit,
            );
            journal.capture_run_context(
                run_id,
                "main",
                model,
                provider,
                self.agent.reasoning_effort(),
                self.agent.prompt_cache_enabled(),
                self.work_dir.to_string_lossy().into_owned(),
                durable_prompt_snapshot(&turn.system_prompt),
                sha256_hex(&tool_schema),
                enabled_tool_names,
                capabilities,
                Some(capability_sha256),
                prompt_template_ids,
                None,
                context_window_limit,
            )?;
        }
        if let Some(path) = self.session_file.clone() {
            self.install_run_trace_recorders(path, run_id.into())?;
        }
        *self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())? = Some(run_id.into());
        Ok(())
    }

    pub(crate) async fn finish_harness_run(
        &mut self,
        run_id: Option<&str>,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        let (Some(journal), Some(run_id)) = (self.harness.as_mut(), run_id) else {
            return Ok(());
        };
        if matches!(
            outcome,
            OperationOutcome::Failed | OperationOutcome::Aborted
        ) {
            if let Some(message) = error
                .as_deref()
                .filter(|message| !message.trim().is_empty())
            {
                journal.append_message(AgentMessage::Custom {
                    custom_type: "agent_error".into(),
                    payload: serde_json::json!({ "error": message }),
                })?;
            }
        }
        let result = journal.finish_run(run_id, outcome, error);
        if result.is_ok() {
            let context = HookContext {
                session_id: journal.store.session_id().to_owned(),
                lane: "main".into(),
                run_id: Some(run_id.into()),
                resume_data: None,
                tool_call_id: None,
                tool_name: None,
                tool_arguments: None,
                tool_result_content: None,
                tool_result_is_error: None,
            };
            for failure in journal
                .store
                .hooks()
                .run(HookKind::AfterRun, &context)
                .await
            {
                warn!("after-run hook {} failed: {}", failure.id, failure.message);
            }
        }
        if let Ok(mut active) = self.harness_run_id.lock() {
            if active.as_deref() == Some(run_id) {
                *active = None;
            }
        }
        self.agent.set_provider_trace_recorder(None);
        self.agent.set_message_recorder(None);
        self.agent.set_provider_boundary_preparer(None);
        self.agent.tool_dispatcher.tool_intent_recorder = None;
        self.agent.tool_dispatcher.tool_execution_trace_recorder = None;
        self.agent.tool_dispatcher.tool_completion_recorder = None;
        self.permission_handle.set_trace_recorder(None);
        result
    }

    pub(crate) fn append_command_message(&mut self, message: AgentMessage) -> Result<(), String> {
        if let Some(journal) = self.harness.as_mut() {
            journal.append_message(message)?;
        }
        Ok(())
    }

    pub(crate) fn prompt_parent_leaf(
        &mut self,
        _message: AgentMessage,
        _harness_persisted: bool,
    ) -> Option<String> {
        self.harness.as_mut().and_then(|journal| {
            let _ = journal.ensure_fresh();
            journal.store.preferred_leaf("main")
        })
    }

    pub(crate) async fn compact_history_with_harness(&mut self) -> Result<bool, String> {
        let before = self.agent.messages().await;
        let compacted = self.agent.preview_compact_history(None).await;
        if compacted == before {
            return Ok(false);
        }
        let summary = compacted
            .iter()
            .rev()
            .find_map(threadlane_runtime::compaction_summary_text)
            .ok_or_else(|| "compaction produced no durable summary".to_string())?
            .to_owned();
        let retained_tail = compaction_retained_tail(&compacted);
        let config = self.agent.config().clone();
        let pre_tokens =
            threadlane_runtime::compaction::estimate_request_tokens(&before, None, &config);
        let compacted_messages = before
            .len()
            .saturating_sub(compacted.len().saturating_sub(1));
        let summary = match self.harness.as_ref() {
            Some(journal) => journal.compaction_summary_without_indexed_tool_outputs(
                &summary,
                compacted_messages,
                &config,
            )?,
            None => summary,
        };
        let persisted = self.persist_harness_compaction(
            &summary,
            &retained_tail,
            pre_tokens,
            compacted_messages,
        );
        // Install only the canonical durable projection, including on a partial
        // append failure. The journal remains authoritative and append-only.
        let sync = self.sync_turn_from_model_context().await;
        persisted?;
        sync?;
        Ok(true)
    }

    pub(crate) fn persist_harness_compaction(
        &mut self,
        summary: &str,
        retained_tail: &[AgentMessage],
        pre_tokens: usize,
        compacted_messages: usize,
    ) -> Result<(), String> {
        let config = self.agent.config().clone();
        let retained_tail_tokens =
            threadlane_runtime::compaction::estimate_request_tokens(retained_tail, None, &config);
        let model = self.agent.model().to_string();
        if let Some(journal) = self.harness.as_mut() {
            journal.ensure_fresh()?;
            let run_id = journal.unique_run_id("foreground-compaction")?;
            let context_snapshot_index =
                journal.context_snapshot_index_for_compaction(compacted_messages)?;
            journal
                .store
                .accept_compaction(&run_id, summary, &context_snapshot_index)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            for message in retained_tail {
                journal.append_message_occurrence(message.clone())?;
            }
            journal.record_manual_compaction(
                &run_id,
                &model,
                &config,
                pre_tokens,
                retained_tail_tokens,
                compacted_messages,
            )?;
        }
        Ok(())
    }

    pub(crate) async fn sync_turn_from_model_context(&self) -> Result<(), String> {
        let Some(harness) = self.harness.as_ref() else {
            return Ok(());
        };
        let context = harness.model_context("main")?;
        let mut turn = self.agent.turn.lock().await;
        let system_prompt = turn.system_prompt.clone();
        turn.messages = std::iter::once(AgentMessage::System {
            content: system_prompt,
        })
        .chain(context.messages())
        .collect();
        Ok(())
    }

    pub(crate) async fn sync_session_history(&mut self) {
        if self.harness.is_some() {
            if let Err(error) = self.sync_turn_from_model_context().await {
                warn!("Failed to project canonical model context: {error}");
            }
        }
    }

    async fn dispatch_assistant_hook(&self, message: &AgentMessage) {
        let AgentMessage::Assistant {
            content,
            tool_calls,
            ..
        } = message
        else {
            return;
        };
        let arguments = serde_json::json!({
            "content": content,
            "tool_calls": tool_calls,
        });
        for response in self
            .wasi_extensions
            .execute_hook_with_effects("assistant_message", &arguments.to_string())
            .into_iter()
            .flatten()
        {
            let _ = dispatch_hook_requests(
                &self.broker_dispatcher,
                &self.wasi_extensions,
                response.host_broker_requests,
            )
            .await;
        }
        let _ = dispatch_hook_requests(
            &self.broker_dispatcher,
            &self.wasi_extensions,
            self.wasi_extensions.take_pending_broker_requests(),
        )
        .await;
    }

    pub(crate) async fn sync_harness_and_dispatch_assistant_hooks(&mut self) {
        let messages = self.agent.messages().await;
        let state_messages: Vec<AgentMessage> = messages
            .into_iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .collect();

        if self.harness.is_some() {
            let durable_messages = self
                .harness
                .as_mut()
                .and_then(|harness| {
                    let _ = harness.ensure_fresh();
                    harness.store.model_context("main").ok()
                })
                .map(|projection| projection.messages())
                .unwrap_or_default();
            if requires_harness_compaction_reset(&durable_messages, &state_messages) {
                let summary = state_messages
                    .iter()
                    .find_map(threadlane_runtime::compaction_summary_text)
                    .expect("compaction reset requires a summary")
                    .to_owned();
                let retained_tail = compaction_retained_tail(&state_messages);
                let config = self.agent.config().clone();
                let pre_tokens = threadlane_runtime::compaction::estimate_request_tokens(
                    &durable_messages,
                    None,
                    &config,
                );
                let compacted_messages = durable_messages
                    .len()
                    .saturating_sub(state_messages.len().saturating_sub(1));
                if let Err(error) = self.persist_harness_compaction(
                    &summary,
                    &retained_tail,
                    pre_tokens,
                    compacted_messages,
                ) {
                    self.harness_journal_error = Some(error);
                    return;
                }
                if let Some(last_assistant) = state_messages
                    .iter()
                    .rev()
                    .find(|message| matches!(message, AgentMessage::Assistant { .. }))
                {
                    self.dispatch_assistant_hook(last_assistant).await;
                }
                return;
            }

            if let Some(harness) = self.harness.as_mut() {
                if let Err(error) = harness.ensure_fresh() {
                    self.harness_journal_error = Some(error);
                    return;
                }
            }
            if let Some(last_assistant) = state_messages
                .iter()
                .rev()
                .find(|message| matches!(message, AgentMessage::Assistant { .. }))
            {
                self.dispatch_assistant_hook(last_assistant).await;
            }
        }
    }

    pub(crate) fn commit_completed_subagent_lanes(&mut self) -> Result<(), String> {
        let lanes = {
            let mut completed = self
                .completed_subagent_lanes
                .lock()
                .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?;
            std::mem::take(&mut *completed)
        };
        for lane in &lanes {
            let status = match lane.status {
                SubagentLaneStatus::Completed => "completed",
                SubagentLaneStatus::Failed => "failed",
            };
            let mut messages = Vec::with_capacity(lane.messages.len() + 1);
            messages.push(AgentMessage::Custom {
                custom_type: "subagent_lane".into(),
                payload: serde_json::json!({
                    "lane": lane.lane_name,
                    "run_id": lane.run_id,
                    "agent": lane.agent,
                    "task": lane.task,
                    "model": lane.model,
                    "status": status,
                    "error": lane.error,
                }),
            });
            messages.extend(lane.messages.clone());
            if let Some(path) = self.session_file.as_deref() {
                let mut journal = CodingSessionHarness::open(path)?;
                for msg in &messages {
                    journal.append_message_to_lane(&lane.lane_name, &lane.run_id, msg.clone())?;
                }
            }
            #[cfg(test)]
            if let Some(observer) = self.subagent_branch_observer.as_ref() {
                observer();
            }
        }
        for (index, lane) in lanes.iter().enumerate() {
            if let Some(path) = self.session_file.as_deref() {
                let outcome = match lane.status {
                    SubagentLaneStatus::Completed => OperationOutcome::Completed,
                    SubagentLaneStatus::Failed => OperationOutcome::Failed,
                };
                let mut journal = CodingSessionHarness::open(path)?;
                if let Err(error) = journal.finish_subagent_lane(
                    &lane.lane_name,
                    &lane.run_id,
                    outcome,
                    lane.error.clone(),
                ) {
                    self.completed_subagent_lanes
                        .lock()
                        .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?
                        .extend_from_slice(&lanes[index..]);
                    self.interrupted_subagent_recovery = InterruptedSubagentRecoveryState::Pending;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn recover_interrupted_subagent_lanes(&mut self) -> Result<usize, String> {
        match &self.interrupted_subagent_recovery {
            InterruptedSubagentRecoveryState::Complete => return Ok(0),
            InterruptedSubagentRecoveryState::Pending => {}
        }
        if let Some(error) = self.harness_journal_error.as_ref() {
            return Err(format!("Harness Error: {error}"));
        }

        let path = self
            .session_file
            .clone()
            .ok_or_else(|| "Interrupted subagent journal is unavailable".to_string())?;
        let records = recover_v2_subagent_records(&path).unwrap_or_default();
        let store = JsonlStore::open_read_only(&path).map_err(|e| e.to_string())?;
        let markers = store
            .entries()
            .iter()
            .filter_map(|entry| match &entry.message {
                AgentMessage::Custom {
                    custom_type,
                    payload,
                } if custom_type == "subagent_lane" => payload
                    .get("run_id")
                    .and_then(Value::as_str)
                    .and_then(|run_id| {
                        payload.get("lane").and_then(Value::as_str).map(|lane| {
                            (
                                (lane.to_owned(), run_id.to_owned()),
                                (
                                    entry.id.clone(),
                                    payload
                                        .get("status")
                                        .and_then(Value::as_str)
                                        .unwrap_or("completed")
                                        .to_owned(),
                                    payload
                                        .get("error")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned),
                                ),
                            )
                        })
                    }),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let mut recovered = 0;

        for lane in threadlane_runtime::interrupted_subagent_lanes(&records) {
            let retrying = |error: String| {
                let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                    run_id: lane.run_id.clone(),
                    status: SubagentRecoveryStatus::Retrying,
                    detail: Some("Recovery needs retry".into()),
                });
                error
            };
            let mut journal = CodingSessionHarness::open(&path).map_err(&retrying)?;
            let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                run_id: lane.run_id.clone(),
                status: SubagentRecoveryStatus::Started,
                detail: Some("Recovering interrupted task".into()),
            });
            if !lane.task_attempted {
                let error = "Interrupted subagent had no persisted task attempt".to_string();
                let messages = vec![AgentMessage::Custom {
                    custom_type: "subagent_lane".into(),
                    payload: serde_json::json!({
                        "lane": lane.lane,
                        "run_id": lane.run_id,
                        "agent": "recovered",
                        "task": lane.task,
                        "status": "aborted",
                        "error": error,
                    }),
                }];
                for message in messages {
                    journal
                        .append_message_to_lane(&lane.lane, &lane.run_id, message)
                        .map_err(&retrying)?;
                }
                journal
                    .finish_subagent_lane(
                        &lane.lane,
                        &lane.run_id,
                        OperationOutcome::Aborted,
                        Some(error),
                    )
                    .map_err(&retrying)?;
                let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                    run_id: lane.run_id.clone(),
                    status: SubagentRecoveryStatus::Aborted,
                    detail: Some("Interrupted task was not replayable".into()),
                });
                recovered += 1;
                continue;
            }
            if let Some((marker_id, status, error)) =
                markers.get(&(lane.lane.clone(), lane.run_id.clone()))
            {
                let recorded = records
                    .iter()
                    .filter_map(|record| match record {
                        HarnessRecord::WriteDeferred {
                            lane: recorded_lane,
                            run_id,
                            target,
                            ..
                        } if *recorded_lane == lane.lane && *run_id == lane.run_id => {
                            serde_json::to_value(target).ok()
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let entries = store.entries();
                let persisted = entries
                    .iter()
                    .filter(|entry| {
                        let mut parent = entry.parent_id.as_deref();
                        while let Some(parent_id) = parent {
                            if parent_id == marker_id {
                                return true;
                            }
                            parent = entries
                                .iter()
                                .find(|e| e.id == parent_id)
                                .and_then(|e| e.parent_id.as_deref());
                        }
                        false
                    })
                    .filter_map(|entry| {
                        (!matches!(entry.message, AgentMessage::Custom { .. }))
                            .then_some(entry.message.clone())
                    })
                    .filter(|message| {
                        serde_json::to_value(message)
                            .ok()
                            .is_some_and(|message| !recorded.contains(&message))
                    })
                    .collect::<Vec<_>>();
                journal
                    .checkpoint(&lane.lane, &lane.run_id, &persisted)
                    .map_err(&retrying)?;
                let outcome = match status.as_str() {
                    "aborted" => OperationOutcome::Aborted,
                    "failed" => OperationOutcome::Failed,
                    _ => OperationOutcome::Completed,
                };
                journal
                    .finish_subagent_lane(&lane.lane, &lane.run_id, outcome, error.clone())
                    .map_err(&retrying)?;
                let (status, detail) = match status.as_str() {
                    "aborted" => (SubagentRecoveryStatus::Aborted, "Recovery was aborted"),
                    "failed" => (SubagentRecoveryStatus::Retrying, "Recovery needs retry"),
                    _ => (SubagentRecoveryStatus::Recovered, "Recovered prior work"),
                };
                let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                    run_id: lane.run_id.clone(),
                    status,
                    detail: Some(detail.into()),
                });
                recovered += 1;
                continue;
            }

            if lane.safe_tools.is_empty()
                && lane.unsafe_tools.is_empty()
                && lane
                    .messages
                    .iter()
                    .any(|message| matches!(message, AgentMessage::Tool { .. }))
            {
                journal
                    .finish_subagent_lane(
                        &lane.lane,
                        &lane.run_id,
                        OperationOutcome::Completed,
                        None,
                    )
                    .map_err(&retrying)?;
                recovered += 1;
                continue;
            }

            let claimed_safe_tools = journal
                .claim_safe_replays(&lane.safe_tools)
                .map_err(&retrying)?;
            let safe_results = self.replay_safe_tools(&claimed_safe_tools).await;
            let safe_messages = safe_results
                .into_iter()
                .map(|result| AgentMessage::Tool {
                    tool_call_id: result.tool_call_id,
                    name: result.name,
                    content: result.content,
                    is_error: result.is_error,
                    terminate: false,
                })
                .collect::<Vec<_>>();
            let unsafe_tool_ids = lane
                .unsafe_tools
                .iter()
                .filter_map(|record| match record {
                    HarnessRecord::ToolStarted { tool_call_id, .. } => Some(tool_call_id.clone()),
                    _ => None,
                })
                .collect::<HashSet<_>>();
            if !unsafe_tool_ids.is_empty() {
                let error = Some("Interrupted unsafe tool execution was not replayed".to_string());
                let mut messages =
                    Vec::with_capacity(1 + lane.messages.len() + safe_messages.len());
                messages.push(AgentMessage::Custom {
                    custom_type: "subagent_lane".into(),
                    payload: serde_json::json!({
                        "lane": lane.lane,
                        "run_id": lane.run_id,
                        "agent": "recovered",
                        "task": lane.task,
                        "status": "aborted",
                        "error": error,
                    }),
                });
                messages.extend(lane.messages.clone());
                messages.extend(safe_messages);
                journal.ensure_fresh().map_err(&retrying)?;
                journal
                    .finish_subagent_lane(
                        &lane.lane,
                        &lane.run_id,
                        OperationOutcome::Aborted,
                        error,
                    )
                    .map_err(&retrying)?;
                let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                    run_id: lane.run_id.clone(),
                    status: SubagentRecoveryStatus::Aborted,
                    detail: Some("Unsafe tool was not replayed".into()),
                });
                recovered += 1;
                continue;
            }

            let mut resume_messages = lane.messages.clone();
            resume_messages.extend(safe_messages);
            let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                run_id: lane.run_id.clone(),
                status: SubagentRecoveryStatus::Retrying,
                detail: Some("Resuming interrupted task".into()),
            });
            let model = self.agent.turn.lock().await.model.clone();
            #[cfg(test)]
            let scheduler_observer = self
                .subagent_work_observer
                .lock()
                .ok()
                .and_then(|observer| observer.clone());
            let identity = SubagentLaneIdentity {
                lane_name: lane.lane.clone(),
                run_id: lane.run_id.clone(),
                source_leaf_id: lane.source_leaf_id.clone(),
                started_seq: lane.started_seq,
            };
            let accepted = journal
                .accepted_subagent_run(&identity)
                .map_err(&retrying)?;
            let result = run_subagent_task(
                AgentDefinition {
                    name: "recovered".into(),
                    description: "Recovered interrupted subagent".into(),
                    tools: None,
                    model: None,
                    system_prompt: "Resume the interrupted child task from its durable checkpoint."
                        .into(),
                    source: crate::agents::AgentSource::Project,
                    file_path: self.work_dir.clone(),
                },
                lane.task.clone(),
                SubagentRunContext {
                    api_key: self.agent.api_key.clone(),
                    account_id: self.agent.account_id.clone(),
                    child_model: self.agent_config.subagent_model.clone().unwrap_or(model),
                    child_reasoning_effort: self
                        .agent_config
                        .subagent_reasoning_effort
                        .unwrap_or_else(|| self.agent.reasoning_effort()),
                    parent_session_id: self.session_id.clone(),
                    work_dir: self.work_dir.clone(),
                    extensions: self.wasi_extensions.clone(),
                    parent_event_tx: self.agent.event_tx.clone(),
                    parent_leaf_id: lane.source_leaf_id.clone(),
                    session_file: self.session_file.clone(),
                    completed_lanes: self.completed_subagent_lanes.clone(),
                    #[cfg(test)]
                    scheduler_observer,
                    #[cfg(test)]
                    child_work_observer: None,
                    #[cfg(test)]
                    child_tool_observer: None,
                    #[cfg(test)]
                    child_run_override: None,
                    semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
                },
                NEXT_SUBAGENT_UI_RUN_ID.fetch_add(1, Ordering::Relaxed),
                0,
                identity,
                Some(accepted),
                resume_messages.clone(),
            )
            .await;
            let (status, outcome, error, resumed_messages) = match result {
                Ok(result) if result.error.is_none() => (
                    "completed",
                    OperationOutcome::Completed,
                    None,
                    result.messages,
                ),
                Ok(result) => (
                    "failed",
                    OperationOutcome::Failed,
                    result.error,
                    result.messages,
                ),
                Err(error) => (
                    "failed",
                    OperationOutcome::Failed,
                    Some(error),
                    resume_messages,
                ),
            };
            let mut messages = Vec::with_capacity(1 + resumed_messages.len());
            messages.push(AgentMessage::Custom {
                custom_type: "subagent_lane".into(),
                payload: serde_json::json!({
                    "lane": lane.lane,
                    "run_id": lane.run_id,
                    "agent": "recovered",
                    "task": lane.task,
                    "status": status,
                    "error": error,
                }),
            });
            messages.extend(resumed_messages);
            journal
                .finish_subagent_lane(&lane.lane, &lane.run_id, outcome, error)
                .map_err(&retrying)?;
            let (status, detail) = if status == "completed" {
                (SubagentRecoveryStatus::Recovered, "Recovery complete")
            } else {
                (SubagentRecoveryStatus::Retrying, "Recovery needs retry")
            };
            let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                run_id: lane.run_id.clone(),
                status,
                detail: Some(detail.into()),
            });
            recovered += 1;
        }
        self.interrupted_subagent_recovery = InterruptedSubagentRecoveryState::Complete;
        Ok(recovered)
    }

    pub(crate) async fn replay_safe_tools(
        &self,
        records: &[threadlane_runtime::Record],
    ) -> Vec<AgentToolResult> {
        let calls = records
            .iter()
            .filter_map(|record| match record {
                threadlane_runtime::Record::ToolStarted {
                    tool_call_id,
                    tool_name,
                    effective_args,
                    replay: threadlane_runtime::ToolReplaySafety::Safe,
                    ..
                } => Some(threadlane_provider::openai::ToolCall {
                    id: tool_call_id.clone(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: tool_name.clone(),
                        arguments: effective_args.to_string(),
                    },
                    thought_signature: None,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        if calls.is_empty() {
            return Vec::new();
        }
        self.agent.execute_tools_for_replay(&calls).await
    }
}
