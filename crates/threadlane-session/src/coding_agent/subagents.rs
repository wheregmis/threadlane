use super::cancellation::AgentRunTask;
use super::capabilities::{
    build_broker_dispatcher, create_after_tool_hook_handler, extension_before_tool_hook_handler,
};
use super::harness::{AcceptedRun, CodingSessionHarness, SubagentLaneIdentity, SubagentStartError};
use super::scheduler::AgentWorkScheduler;
#[cfg(test)]
use super::scheduler::{
    AgentWork, AgentWorkObserver, DeterministicSubagentToolExecutor, SubagentBoundaryObserver,
};
use crate::agents::{discover_agents, AgentDefinition, AgentScope};
use crate::policy::ToolPolicy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use threadlane_runtime::harness::HookKind;
use threadlane_runtime::{
    AgentEvent, AgentMessage, AgentRuntime, SubagentProgressUpdate, TurnState,
};
use threadlane_wasi::WasiExtensionManager;
use tokio::sync::broadcast;
use tokio::time::{timeout, Duration};

pub(crate) const MAX_SUBAGENT_TASKS: usize = 8;
pub(crate) const MAX_SUBAGENT_TASK_CHARS: usize = 32_000;
pub(crate) const SUBAGENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub(crate) const SUBAGENT_RECOVERY_PROMPT: &str =
    "Continue from the recovered checkpoint and finish the assigned task.";
pub(crate) static NEXT_SUBAGENT_UI_RUN_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) type AgentRunner = Arc<
    dyn Fn(
            Vec<AgentRunTask>,
            bool,
            Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub(crate) enum SubagentLaneStatus {
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedSubagentLane {
    pub(crate) lane_name: String,
    pub(crate) run_id: String,
    pub(crate) task: String,
    pub(crate) agent: String,
    pub(crate) model: String,
    pub(crate) status: SubagentLaneStatus,
    pub(crate) messages: Vec<AgentMessage>,
    pub(crate) error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct SubagentRunContext {
    pub(crate) api_key: String,
    pub(crate) account_id: Option<String>,
    pub(crate) child_model: String,
    pub(crate) child_reasoning_effort: threadlane_runtime::ReasoningEffort,
    pub(crate) parent_session_id: String,
    pub(crate) work_dir: PathBuf,
    pub(crate) extensions: Arc<WasiExtensionManager>,
    pub(crate) parent_event_tx: broadcast::Sender<AgentEvent>,
    pub(crate) parent_leaf_id: Option<String>,
    pub(crate) session_file: Option<PathBuf>,
    #[cfg(test)]
    pub(crate) scheduler_observer: Option<AgentWorkObserver>,
    #[cfg(test)]
    pub(crate) child_work_observer: Option<SubagentBoundaryObserver>,
    #[cfg(test)]
    pub(crate) child_tool_observer: Option<Arc<AtomicBool>>,
    pub(crate) semaphore: Arc<tokio::sync::Semaphore>,
}

#[derive(Clone, Debug)]
pub struct SubagentInnerTool {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub output: String,
    pub is_error: bool,
}

#[derive(Clone, Debug)]
pub struct SubagentResult {
    pub output: String,
    pub thinking: Vec<AgentMessage>,
    pub inner_tools: Vec<SubagentInnerTool>,
    pub error: Option<String>,
    pub messages: Vec<AgentMessage>,
}

pub(crate) fn tool_target_preview(name: &str, arguments: &str) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(arguments).ok();
    let get_str = |key: &str| {
        parsed
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_str)
    };
    let target = match name {
        "read_file" | "write_file" | "edit_file" | "edit_file_hashline" => get_str("path")
            .or_else(|| get_str("file_path"))
            .unwrap_or(arguments),
        "list_dir" => get_str("path").unwrap_or(arguments),
        "run_command" => get_str("command").unwrap_or(arguments),
        _ => arguments,
    };
    if target.chars().count() > 60 {
        target.chars().take(60).collect::<String>()
    } else {
        target.to_string()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentSessionData {
    pub run_id: String,
    pub task: String,
    pub agent: String,
    pub status: String,
    pub thinking: String,
    pub inner_tools: Vec<SubagentInnerToolData>,
    pub output: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentInnerToolData {
    pub name: String,
    pub target_preview: String,
    pub is_error: bool,
}

pub(crate) fn format_subagent_results(
    tasks: Vec<AgentRunTask>,
    results: Vec<Result<SubagentResult, String>>,
    lanes: &[CompletedSubagentLane],
) -> String {
    let sessions: Vec<SubagentSessionData> = tasks
        .into_iter()
        .zip(results)
        .zip(lanes)
        .map(|((task, result), lane)| match result {
            Ok(res) => {
                let mut thinking = String::new();
                for think_msg in &res.thinking {
                    if let AgentMessage::Custom { payload, .. } = think_msg {
                        if let Some(thought) = payload.get("thought").and_then(|v| v.as_str()) {
                            thinking.push_str(thought);
                            thinking.push('\n');
                        }
                    }
                }
                let inner_tools = res
                    .inner_tools
                    .into_iter()
                    .map(|tool| SubagentInnerToolData {
                        name: tool.name.clone(),
                        target_preview: tool_target_preview(&tool.name, &tool.arguments),
                        is_error: tool.is_error,
                    })
                    .collect();
                let status = match lane.status {
                    SubagentLaneStatus::Completed => "completed",
                    SubagentLaneStatus::Failed => "failed",
                };
                SubagentSessionData {
                    run_id: lane.run_id.clone(),
                    task: task.task,
                    agent: task.agent,
                    status: status.to_string(),
                    thinking,
                    inner_tools,
                    output: res.output,
                }
            }
            Err(err) => SubagentSessionData {
                run_id: lane.run_id.clone(),
                task: task.task,
                agent: task.agent,
                status: "failed".to_string(),
                thinking: String::new(),
                inner_tools: Vec::new(),
                output: format!("Subagent failed to run: {err}"),
            },
        })
        .collect();

    serde_json::to_string(&sessions).unwrap_or_else(|_| "[]".to_string())
}

pub(crate) fn aggregate_subagent_results(
    tasks: Vec<AgentRunTask>,
    results: Vec<Result<SubagentResult, String>>,
    lanes: Vec<CompletedSubagentLane>,
) -> Result<(String, Vec<AgentMessage>, Vec<CompletedSubagentLane>), String> {
    let any_succeeded = results
        .iter()
        .any(|result| matches!(result, Ok(result) if result.error.is_none()));
    let thinking = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .flat_map(|result| result.thinking.clone())
        .collect();
    let output = format_subagent_results(tasks, results, &lanes);
    if any_succeeded {
        Ok((output, thinking, lanes))
    } else {
        Err(format!("All subagents failed: {output}"))
    }
}
pub(crate) fn subagent_ui_event(
    event: AgentEvent,
    run_id: u64,
    task_index: usize,
    journal_run_id: &str,
    lane: &str,
    tool_call_prefix: &str,
) -> Option<AgentEvent> {
    let update = match event {
        AgentEvent::MessageUpdate {
            text_delta: Some(delta),
            ..
        } => SubagentProgressUpdate::TextDelta { delta },
        AgentEvent::MessageUpdate {
            reasoning_delta: Some(delta),
            ..
        } => SubagentProgressUpdate::ReasoningDelta { delta },
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            name,
            arguments,
        } => SubagentProgressUpdate::ToolStarted {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            name,
            arguments,
        },
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
        } => SubagentProgressUpdate::ToolUpdated {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            partial_result,
        },
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            name,
            result,
        } => SubagentProgressUpdate::ToolFinished {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            name,
            result,
        },
        AgentEvent::AgentError { error } => SubagentProgressUpdate::Error { error },
        _ => return None,
    };
    Some(AgentEvent::SubagentUpdate {
        run_id,
        task_index,
        journal_run_id: journal_run_id.to_owned(),
        lane: lane.to_owned(),
        update,
    })
}

pub(crate) async fn checkpoint_new_subagent_messages(
    session_file: Option<&Path>,
    lane_name: &str,
    run_id: &str,
    state: &Arc<tokio::sync::Mutex<TurnState>>,
    checkpoint_cursor: &mut usize,
) -> Result<(), String> {
    let messages = state.lock().await.messages.clone();
    if let Some(path) = session_file {
        let mut journal = CodingSessionHarness::open(path)?;
        journal.checkpoint(lane_name, run_id, &messages[*checkpoint_cursor..])?;
    }
    *checkpoint_cursor = messages.len();
    Ok(())
}

pub(crate) async fn consume_subagent_turn_checkpoints(
    mut events: broadcast::Receiver<AgentEvent>,
    session_file: Option<PathBuf>,
    lane_name: String,
    run_id: String,
    state: Arc<tokio::sync::Mutex<TurnState>>,
    initial_checkpoint_cursor: usize,
) -> Result<usize, String> {
    let mut checkpoint_cursor = initial_checkpoint_cursor;
    while let Ok(event) = events.recv().await {
        if matches!(&event, AgentEvent::TurnEnd { .. }) {
            checkpoint_new_subagent_messages(
                session_file.as_deref(),
                &lane_name,
                &run_id,
                &state,
                &mut checkpoint_cursor,
            )
            .await?;
        }
        if matches!(&event, AgentEvent::AgentEnd { .. }) {
            break;
        }
    }
    Ok(checkpoint_cursor)
}

pub(crate) async fn checkpoint_subagent_final_snapshot(
    session_file: Option<&Path>,
    lane_name: &str,
    run_id: &str,
    state: &Arc<tokio::sync::Mutex<TurnState>>,
    checkpoint_cursor: &mut usize,
) -> Result<(), String> {
    checkpoint_new_subagent_messages(session_file, lane_name, run_id, state, checkpoint_cursor)
        .await
}

pub(crate) fn accept_completed_subagent_lanes(
    completed_lanes: &Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
    lanes: Vec<CompletedSubagentLane>,
) -> Result<(), String> {
    completed_lanes
        .lock()
        .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?
        .extend(lanes);
    Ok(())
}

pub(crate) async fn run_subagents_with_context(
    tasks: Vec<AgentRunTask>,
    parallel: bool,
    tool_call_id: Option<String>,
    context: SubagentRunContext,
) -> Result<(String, Vec<AgentMessage>, Vec<CompletedSubagentLane>), String> {
    let run_id = NEXT_SUBAGENT_UI_RUN_ID.fetch_add(1, Ordering::Relaxed);
    log::info!(
        "subagent batch run_id={}: {} task(s), parallel={parallel}",
        run_id,
        tasks.len()
    );
    for (task_index, task) in tasks.iter().enumerate() {
        log::debug!(
            "subagent queued run_id={run_id} task_index={task_index} agent={} task={}",
            task.agent,
            task.task
        );
        let _ = context.parent_event_tx.send(AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent: task.agent.clone(),
            task: task.task.clone(),
        });
    }
    let candidates = discover_agents(&context.work_dir, AgentScope::Both).agents;
    let lane_key = tool_call_id
        .map(|id| format!("tool-{id}"))
        .unwrap_or_else(|| "explicit".into());
    let run_one = |task_index: usize, task: AgentRunTask| {
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.name == task.agent)
            .cloned();

        let mut config = match candidate {
            Some(static_config) => static_config,
            None => {
                let sys_prompt = task.instructions.clone().unwrap_or_else(|| {
                    format!(
                        "You are a specialized subagent acting as {}. Complete only the assigned task and report results clearly to the parent agent.",
                        task.agent
                    )
                });
                AgentDefinition {
                    name: task.agent.clone(),
                    description: format!("Dynamic subagent for {}", task.agent),
                    tools: task.tools.clone(),
                    model: None,
                    system_prompt: sys_prompt,
                    source: crate::agents::AgentSource::Project,
                    file_path: context.work_dir.clone(),
                }
            }
        };
        if let Some(inst) = &task.instructions {
            config.system_prompt = inst.clone();
        }
        if let Some(t) = &task.tools {
            config.tools = Some(t.clone());
        }

        let context = context.clone();
        let event_tx = context.parent_event_tx.clone();
        let lane_task = task.task.clone();
        let lane_agent = task.agent.clone();
        let lane_key = lane_key.clone();
        async move {
            let parent_leaf_id = context.parent_leaf_id.clone();
            let lane_hint = format!(
                "subagent-{}-{}:{task_index}",
                context.parent_session_id, lane_key
            );
            let _permit = context
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| "Subagent concurrency limiter closed".to_string())?;
            let start = match context.session_file.as_deref() {
                Some(path) => {
                    let mut journal = CodingSessionHarness::open(path)?;
                    let result = journal.start_subagent_lane(
                        &lane_hint,
                        &task.task,
                        parent_leaf_id.as_deref(),
                    );
                    match &result {
                        Ok(started) => log::info!(
                            "subagent lane started: run_id={} lane={}",
                            started.identity.run_id,
                            started.identity.lane_name
                        ),
                        Err(e) => log::warn!(
                            "subagent lane start failed: hint={lane_hint} error={}",
                            e.error
                        ),
                    }
                    result.map(|started| (started.identity, Some(started.accepted)))
                }
                None => {
                    log::warn!(
                        "subagent lane={lane_hint}: no session_file, running without harness"
                    );
                    Ok((
                        SubagentLaneIdentity {
                            lane_name: lane_hint.clone(),
                            run_id: lane_hint.clone(),
                            source_leaf_id: parent_leaf_id.clone(),
                            started_seq: 0,
                        },
                        None,
                    ))
                }
            };
            let resolved_model = context.child_model.clone();
            let result = match start {
                Ok((identity, accepted)) => {
                    let _ = event_tx.send(AgentEvent::SubagentStarted {
                        run_id,
                        task_index,
                        journal_run_id: identity.run_id.clone(),
                        lane: identity.lane_name.clone(),
                        agent: lane_agent.clone(),
                        task: lane_task.clone(),
                        model: resolved_model.clone(),
                    });
                    #[cfg(test)]
                    if let Some(observer) = context.child_work_observer.as_ref() {
                        observer();
                    }
                    timeout(
                        SUBAGENT_TIMEOUT,
                        run_subagent_task(
                            config,
                            task.task,
                            context,
                            run_id,
                            task_index,
                            identity.clone(),
                            accepted,
                            Vec::new(),
                        ),
                    )
                    .await
                    .map_err(|_| "Subagent timed out".to_string())
                    .map(|result| (result, identity))?
                }
                Err(SubagentStartError { identity, error }) => (
                    Err(error),
                    identity.unwrap_or_else(|| SubagentLaneIdentity {
                        lane_name: lane_hint.clone(),
                        run_id: lane_hint,
                        source_leaf_id: parent_leaf_id.clone(),
                        started_seq: 0,
                    }),
                ),
            };
            let (result, identity) = result;
            let (succeeded, error) = match &result {
                Ok(result) if result.error.is_none() => (true, None),
                Ok(result) => (false, result.error.clone()),
                Err(error) => (false, Some(error.clone())),
            };
            log::info!(
                "subagent finished run_id={} journal_run_id={} succeeded={succeeded}",
                run_id,
                identity.run_id
            );
            let _ = event_tx.send(AgentEvent::SubagentFinished {
                run_id,
                task_index,
                journal_run_id: identity.run_id.clone(),
                succeeded,
                error,
            });
            let lane = CompletedSubagentLane {
                lane_name: identity.lane_name,
                run_id: identity.run_id,
                task: lane_task,
                agent: lane_agent,
                model: resolved_model,
                status: if succeeded {
                    SubagentLaneStatus::Completed
                } else {
                    SubagentLaneStatus::Failed
                },
                messages: result
                    .as_ref()
                    .map(|result| result.messages.clone())
                    .unwrap_or_default(),
                error: result
                    .as_ref()
                    .ok()
                    .and_then(|result| result.error.clone())
                    .or_else(|| result.as_ref().err().cloned()),
            };
            Ok((result, lane))
        }
    };
    let results = if parallel {
        futures::future::join_all(
            tasks
                .iter()
                .cloned()
                .enumerate()
                .map(|(task_index, task)| run_one(task_index, task)),
        )
        .await
    } else {
        let mut previous = String::new();
        let mut results = Vec::with_capacity(tasks.len());
        for (task_index, task) in tasks.iter().cloned().enumerate() {
            let task = AgentRunTask {
                agent: task.agent,
                task: task.task.replace("{previous}", &previous),
                instructions: task.instructions,
                tools: task.tools,
                model: task.model,
            };
            let result = run_one(task_index, task).await?;
            if let Ok(output) = &result.0 {
                previous = output.output.clone();
            }
            results.push(Ok(result));
        }
        results
    };
    let results = results.into_iter().collect::<Result<Vec<_>, String>>()?;
    let (tool_results, lanes): (Vec<_>, Vec<_>) = results.into_iter().unzip();
    aggregate_subagent_results(tasks, tool_results, lanes)
}

pub(crate) async fn run_subagent_task(
    config: AgentDefinition,
    task: String,
    context: SubagentRunContext,
    run_id: u64,
    task_index: usize,
    identity: SubagentLaneIdentity,
    accepted: Option<AcceptedRun>,
    resume_messages: Vec<AgentMessage>,
) -> Result<SubagentResult, String> {
    let model = context.child_model.clone();
    let lane_name = identity.lane_name.clone();
    let journal_run_id = identity.run_id.clone();
    let subagent_session = context
        .session_file
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("subagent-{lane_name}.jsonl")));
    let mut agent = AgentRuntime::new_with_provider(
        context.api_key.clone(),
        context.account_id.clone(),
        &model,
        Some(&subagent_session),
        threadlane_runtime::AgentConfig::default(),
        Arc::new(threadlane_provider::router::ProviderClient::new(
            context.api_key.clone(),
            context.account_id.clone(),
        )),
    )
    .unwrap();
    agent
        .set_reasoning_effort(context.child_reasoning_effort)
        .await;

    if let Some(tools) = config.tools.clone() {
        agent.set_allowed_tool_names(Some(tools.into_iter().collect()));
    }
    let system_prompt = format!(
        "{}

You are an isolated subagent working in {}. Complete only the assigned task and return a concise final report to your parent agent.",
        config.system_prompt,
        context.work_dir.display(),
    );
    agent.set_system_prompt(system_prompt).await;
    let is_recovery = !resume_messages.is_empty();
    if is_recovery {
        agent.turn.lock().await.messages.extend(
            resume_messages
                .iter()
                .filter(|message| !matches!(message, AgentMessage::System { .. }))
                .cloned(),
        );
    } else if context.session_file.is_some() {
        agent
            .sync_turn_from_model_context_on_lane(&lane_name)
            .await
            .map_err(|error| format!("Failed to load subagent lane context: {error}"))?;
    }
    agent.work_dir = Some(context.work_dir.clone());

    let session_file_for_checkpoint = context.session_file.clone();

    let policy = Arc::new(tokio::sync::Mutex::new(
        if config.tools.as_ref().is_some_and(|tools| {
            !tools
                .iter()
                .any(|tool| matches!(tool.as_str(), "write_file" | "edit_file" | "write" | "edit"))
        }) {
            ToolPolicy::ReadOnly
        } else {
            ToolPolicy::FullAccess
        },
    ));
    let agent_work = AgentWorkScheduler::default();
    let (broker_dispatcher, _, _) = build_broker_dispatcher(
        policy.clone(),
        context.extensions.clone(),
        false,
        context.work_dir.clone(),
        agent.event_tx.clone(),
        agent_work.clone(),
        None,
        Some(subagent_session.clone()),
    );
    agent
        .hook_registry
        .register(
            HookKind::BeforeTool,
            "extension-before-tool",
            extension_before_tool_hook_handler(
                policy,
                context.extensions.clone(),
                broker_dispatcher.clone(),
            ),
        )
        .expect("extension before-tool hook must register");
    agent
        .hook_registry
        .register(
            HookKind::AfterTool,
            "extension-after-tool",
            create_after_tool_hook_handler(context.extensions.clone(), broker_dispatcher),
        )
        .expect("extension after-tool hook must register");

    #[cfg(test)]
    if let Some(observer) = context.scheduler_observer.as_ref() {
        if is_recovery
            && resume_messages
                .iter()
                .any(|message| matches!(message, AgentMessage::Tool { .. }))
        {
            return Ok(SubagentResult {
                output: "test subagent result".into(),
                thinking: Vec::new(),
                inner_tools: Vec::new(),
                error: None,
                messages: resume_messages,
            });
        }
        if let Some(tool_observer) = context.child_tool_observer.as_ref() {
            agent
                .register_tool_executor(Arc::new(DeterministicSubagentToolExecutor {
                    observed: tool_observer.clone(),
                }))
                .map_err(|e| e.to_string())?;
            let tool_results = agent
                .execute_tools(&[threadlane_provider::openai::ToolCall {
                    id: "test-child-tool".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "test_child_tool".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                }])
                .await;
            if tool_results[0].is_error {
                return Err(tool_results[0].content.clone());
            }
        }
        let scheduler = AgentWorkScheduler::default();
        scheduler.set_test_observer(observer.clone());
        scheduler.schedule(if is_recovery {
            AgentWork::QueueMessage {
                content: SUBAGENT_RECOVERY_PROMPT.into(),
                images: Vec::new(),
            }
        } else {
            AgentWork::QueueMessage {
                content: "test subagent follow-up".into(),
                images: Vec::new(),
            }
        });
        let observed_model = model.clone();
        let _ = scheduler.run_executor(&mut agent, None).await;
        let mut messages = if is_recovery {
            resume_messages.clone()
        } else {
            vec![AgentMessage::User {
                content: task.clone(),
            }]
        };
        messages.push(AgentMessage::Assistant {
            content: Some("test subagent result".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });
        return Ok(SubagentResult {
            output: format!("test subagent result ({observed_model})"),
            thinking: Vec::new(),
            inner_tools: Vec::new(),
            error: None,
            messages,
        });
    }

    let mut ui_events = agent.subscribe();
    let ui_event_prefix = format!("subagent-{run_id}:{task_index}:",);
    let ui_journal_run_id = journal_run_id.clone();
    let ui_lane_name = lane_name.clone();
    let event_tx_clone = context.parent_event_tx.clone();
    tokio::spawn(async move {
        while let Ok(event) = ui_events.recv().await {
            if let Some(event) = subagent_ui_event(
                event,
                run_id,
                task_index,
                &ui_journal_run_id,
                &ui_lane_name,
                &ui_event_prefix,
            ) {
                let _ = event_tx_clone.send(event);
            }
        }
    });

    let checkpoint_events = agent.subscribe();
    let checkpoint_state = agent.turn.clone();
    let checkpoint_session_file = session_file_for_checkpoint.clone();
    let checkpoint_lane_name = lane_name.clone();
    let checkpoint_run_id = journal_run_id.clone();
    let initial_checkpoint_cursor = agent.turn.lock().await.messages.len();
    let checkpoint_task = tokio::spawn(consume_subagent_turn_checkpoints(
        checkpoint_events,
        checkpoint_session_file,
        checkpoint_lane_name,
        checkpoint_run_id,
        checkpoint_state,
        initial_checkpoint_cursor,
    ));

    let mut events = agent.subscribe();
    let prompt_text = if is_recovery {
        SUBAGENT_RECOVERY_PROMPT
    } else {
        &task
    };
    if let Some(session_path) = session_file_for_checkpoint.as_deref() {
        let accepted_run = accepted.as_ref().ok_or_else(|| {
            format!(
                "Missing accepted subagent run for lane {} ({})",
                lane_name, journal_run_id
            )
        })?;
        let subagent_harness = CodingSessionHarness::open(session_path)
            .map_err(|error| format!("Failed to open subagent harness: {error}"))?;
        subagent_harness
            .validate_accepted_run(accepted_run)
            .map_err(|error| format!("Invalid subagent accepted run token: {error}"))?;
        agent
            .run_accepted(
                &accepted_run.run_id,
                &accepted_run.lane,
                accepted_run.accepted_through_seq,
            )
            .await;
    } else {
        agent.steer(AgentMessage::user(prompt_text.to_string(), Vec::new()));
        agent.run_steer().await;
    }
    while agent_work.run_executor(&mut agent, None).await {}

    let mut checkpoint_cursor = checkpoint_task
        .await
        .map_err(|error| format!("Child turn checkpoint task failed: {error}"))??;
    checkpoint_subagent_final_snapshot(
        session_file_for_checkpoint.as_deref(),
        &lane_name,
        &journal_run_id,
        &agent.turn,
        &mut checkpoint_cursor,
    )
    .await?;

    let mut error = None;
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::AgentError { error: message } = event {
            error = Some(message);
        }
    }
    let state = agent.get_state().await;
    let output = state
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            AgentMessage::Assistant {
                content: Some(content),
                ..
            } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let thinking: Vec<AgentMessage> = state
        .messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::Custom { custom_type, .. } if custom_type == "thinking"))
        .cloned()
        .collect();
    let mut inner_tools = Vec::new();
    for message in &state.messages {
        match message {
            AgentMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                for call in calls {
                    inner_tools.push(SubagentInnerTool {
                        id: call.id.clone(),
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                        output: String::new(),
                        is_error: false,
                    });
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                content,
                is_error,
                ..
            } => {
                if let Some(tool) = inner_tools.iter_mut().find(|t| &t.id == tool_call_id) {
                    tool.output = content.clone();
                    tool.is_error = *is_error;
                }
            }
            _ => {}
        }
    }
    let completion_error = error
        .map(|error| format!("Subagent '{}' failed: {error}", config.name))
        .or_else(|| {
            output.is_empty().then(|| {
                format!(
                    "Subagent '{}' completed without a final text response.",
                    config.name
                )
            })
        });
    Ok(SubagentResult {
        output: completion_error.clone().unwrap_or(output),
        thinking,
        inner_tools,
        error: completion_error,
        messages: state
            .messages
            .into_iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .collect(),
    })
}

#[cfg(test)]
mod result_tests {
    use super::*;

    fn task(agent: &str) -> AgentRunTask {
        AgentRunTask {
            agent: agent.into(),
            task: format!("{agent} task"),
            instructions: None,
            tools: None,
            model: None,
        }
    }

    fn lane(agent: &str, status: SubagentLaneStatus) -> CompletedSubagentLane {
        CompletedSubagentLane {
            lane_name: format!("lane-{agent}"),
            run_id: format!("run-{agent}"),
            task: format!("{agent} task"),
            agent: agent.into(),
            model: "test-model".into(),
            status,
            messages: Vec::new(),
            error: None,
        }
    }

    fn success(output: &str) -> SubagentResult {
        SubagentResult {
            output: output.into(),
            thinking: Vec::new(),
            inner_tools: Vec::new(),
            error: None,
            messages: Vec::new(),
        }
    }

    #[test]
    fn subagent_ui_event_preserves_identity_and_stream_content() {
        let event = subagent_ui_event(
            AgentEvent::MessageUpdate {
                text_delta: Some("working".into()),
                reasoning_delta: None,
                tool_call_name: None,
            },
            7,
            2,
            "journal-run",
            "child-lane",
            "subagent-7:2:",
        )
        .unwrap();

        assert_eq!(
            event,
            AgentEvent::SubagentUpdate {
                run_id: 7,
                task_index: 2,
                journal_run_id: "journal-run".into(),
                lane: "child-lane".into(),
                update: SubagentProgressUpdate::TextDelta {
                    delta: "working".into(),
                },
            }
        );
    }

    #[test]
    fn subagent_ui_event_scopes_tool_ids_without_parsing_them() {
        let event = subagent_ui_event(
            AgentEvent::ToolExecutionStart {
                tool_call_id: "provider:id:with:colons".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            },
            4,
            1,
            "journal-run",
            "child-lane",
            "subagent-4:1:",
        )
        .unwrap();

        let AgentEvent::SubagentUpdate { update, .. } = event else {
            panic!("expected a subagent update");
        };
        assert!(matches!(
            update,
            SubagentProgressUpdate::ToolStarted { tool_call_id, .. }
                if tool_call_id == "subagent-4:1:provider:id:with:colons"
        ));
    }

    #[test]
    fn all_failed_subagent_batch_returns_error_with_each_failure() {
        let result = aggregate_subagent_results(
            vec![task("worker"), task("reviewer")],
            vec![
                Err("worker unavailable".into()),
                Err("review rejected".into()),
            ],
            vec![
                lane("worker", SubagentLaneStatus::Failed),
                lane("reviewer", SubagentLaneStatus::Failed),
            ],
        );

        let error = result.unwrap_err();
        assert!(error.contains("worker unavailable"));
        assert!(error.contains("review rejected"));
        assert!(error.contains("\"status\":\"failed\""));
    }

    #[test]
    fn mixed_subagent_batch_succeeds_with_explicit_statuses() {
        let result = aggregate_subagent_results(
            vec![task("worker"), task("reviewer")],
            vec![Ok(success("implemented")), Err("review rejected".into())],
            vec![
                lane("worker", SubagentLaneStatus::Completed),
                lane("reviewer", SubagentLaneStatus::Failed),
            ],
        )
        .unwrap();

        assert!(result.0.contains("implemented"));
        assert!(result.0.contains("review rejected"));
        assert!(result.0.contains("\"status\":\"completed\""));
        assert!(result.0.contains("\"status\":\"failed\""));
    }
}
