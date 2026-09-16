use std::path::{Path, PathBuf};
use threadlane_runtime::harness::{JsonlStore, SessionStore};
use threadlane_protocol::AgentMessage;

use crate::types::{
    ChatMessageInfo, MessageRole, SessionProjectionResult, SubagentActivityInfo,
    SubagentActivityStatus, ToolActivityInfo,
};
use crate::AppState;

pub use threadlane_runtime::titles::extract_session_title;

#[cfg(test)]
pub fn load_session_messages(session_file: &Path) -> Vec<ChatMessageInfo> {
    compute_session_messages(session_file).unwrap_or_default()
}

pub fn compute_session_messages(
    session_file: &Path,
) -> Result<Vec<ChatMessageInfo>, String> {
    use threadlane_runtime::harness::{read_transcript_page, TranscriptItem};

    // The durable pager is the single transcript source, but exhaust it here:
    // GPUI state continues to expose complete chronological history.
    let mut cursor = None;
    let mut pages = Vec::new();
    loop {
        let page =
            read_transcript_page(session_file, cursor, 40).map_err(|error| error.to_string())?;
        let has_older = page.has_older;
        cursor = page.next_cursor;
        pages.push(page.items);
        if !has_older {
            break;
        }
    }
    pages.reverse();
    let items = pages.into_iter().flatten().collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut messages = Vec::new();
    let mut segment_start = 0usize;
    let flush = |messages: &mut Vec<AgentMessage>, rows: &mut Vec<ChatMessageInfo>, start| {
        for (index, mut row) in project_agent_messages(std::mem::take(messages))
            .into_iter()
            .enumerate()
        {
            row.id = format!("history-{start}-{index}-{}", row.id);
            rows.push(row);
        }
    };
    for (item_index, item) in items.into_iter().enumerate() {
        match item {
            TranscriptItem::Message(message) => {
                if messages.is_empty() {
                    segment_start = item_index;
                }
                messages.push(message);
            }
            TranscriptItem::ContextCompacted(marker) => {
                flush(&mut messages, &mut rows, segment_start);
                rows.push(ChatMessageInfo {
                    id: format!("history-context-{}", marker.seq),
                    role: MessageRole::ContextMarker,
                    content: format!(
                        "Context compacted · {} → {}",
                        format_context_marker_tokens(marker.pre_tokens),
                        format_context_marker_tokens(marker.post_tokens),
                    ),
                    tool_activities: Vec::new(),
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                });
            }
        }
    }
    flush(&mut messages, &mut rows, segment_start);
    Ok(rows)
}

/// Opens a session JSONL once and builds every UI projection required after hydration.
pub fn compute_full_session_projection(
    session_file: &Path,
) -> Result<SessionProjectionResult, String> {
    let store = JsonlStore::open_read_only(session_file).map_err(|error| error.to_string())?;
    let diagnostics = threadlane_runtime::harness::project_session_diagnostics(&store, "main")
        .map_err(|error| error.to_string())?;
    let (trajectory, metrics, token_usage, context_window) =
        AppState::project_trajectory_from_store(&store);
    let subagents = project_subagents_from_store(&store);
    Ok(SessionProjectionResult {
        plan: store.plan(),
        trajectory,
        subagents,
        diagnostics,
        metrics,
        token_usage,
        context_window,
    })
}

pub fn project_subagents_from_store(store: &impl SessionStore) -> Vec<SubagentActivityInfo> {
    use threadlane_runtime::harness::{Record, SubagentLifecyclePhase};

    let mut rows = Vec::new();
    for lane in store.lanes().into_iter().filter(|lane| lane != "main") {
        let has_subagent_lifecycle = store.records().iter().any(|record| {
            matches!(
                record,
                Record::SubagentLifecycle { subagent_lane, .. }
                    if subagent_lane.as_str() == lane
            )
        });
        let transcript = store.transcript(&lane);
        let has_subagent_marker = transcript.entries.iter().any(|entry| {
            matches!(
                &entry.message,
                AgentMessage::Custom { custom_type, .. } if custom_type == "subagent_lane"
            )
        });
        if !has_subagent_lifecycle && !has_subagent_marker {
            continue;
        }
        let mut run_id = String::new();
        let mut agent = lane.clone();
        let mut task = String::new();
        let mut model = None;
        let mut status = SubagentActivityStatus::Running;
        let mut error = None;
        let mut messages = Vec::new();
        for entry in transcript.entries {
            match entry.message {
                AgentMessage::Custom {
                    custom_type,
                    payload,
                } if custom_type == "subagent_lane" => {
                    run_id = payload
                        .get("run_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    agent = payload
                        .get("agent")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(&agent)
                        .to_owned();
                    task = payload
                        .get("task")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    model = payload
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    error = payload
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    status = match payload.get("status").and_then(serde_json::Value::as_str) {
                        Some("completed") => SubagentActivityStatus::Completed,
                        Some("failed") => SubagentActivityStatus::Failed,
                        _ => SubagentActivityStatus::Running,
                    };
                }
                message => messages.push(message),
            }
        }
        let latest = store
            .records()
            .iter()
            .filter_map(|record| match record {
                Record::SubagentLifecycle {
                    seq,
                    child_run_id,
                    agent_id,
                    subagent_lane,
                    phase,
                    error,
                    ..
                } if subagent_lane.as_str() == lane => Some((
                    *seq,
                    child_run_id.as_str(),
                    agent_id.as_str(),
                    phase,
                    error.as_ref().map(|error| error.as_str()),
                )),
                _ => None,
            })
            .max_by_key(|item| item.0);
        if let Some((_, durable_run_id, durable_agent, phase, durable_error)) = latest {
            run_id = durable_run_id.to_owned();
            if agent == lane {
                agent = durable_agent.to_owned();
            }
            status = match phase {
                SubagentLifecyclePhase::Spawned => SubagentActivityStatus::Queued,
                SubagentLifecyclePhase::Started => SubagentActivityStatus::Running,
                SubagentLifecyclePhase::Completed => SubagentActivityStatus::Completed,
                SubagentLifecyclePhase::Failed => SubagentActivityStatus::Failed,
                SubagentLifecyclePhase::Cancelled => SubagentActivityStatus::Cancelled,
            };
            if durable_error.is_some() {
                error = durable_error.map(str::to_owned);
            }
        }
        if run_id.is_empty() {
            run_id = lane.clone();
        }
        rows.push(SubagentActivityInfo {
            batch_run_id: 0,
            task_index: rows.len(),
            journal_run_id: Some(run_id),
            lane: Some(lane),
            agent,
            task,
            model,
            status,
            messages: project_agent_messages(messages),
            error,
            isolation: None,
        });
    }
    rows
}

pub use threadlane_runtime::harness::tool_activity_summary;

pub use threadlane_runtime::harness::tool_activity_display_summary;

pub fn format_context_marker_tokens(tokens: usize) -> String {
    let formatted = threadlane_ui_catalog::format_tokens(tokens.min(u32::MAX as usize) as u32);
    formatted.replace(".0k", "k").replace(".0M", "M")
}

pub fn project_agent_messages(agent_messages: Vec<AgentMessage>) -> Vec<ChatMessageInfo> {
    threadlane_runtime::harness::project_chat_messages(&agent_messages)
        .into_iter()
        .map(|msg| ChatMessageInfo {
            id: msg.id,
            role: match msg.role {
                threadlane_runtime::harness::UiMessageRole::User => MessageRole::User,
                threadlane_runtime::harness::UiMessageRole::Assistant => MessageRole::Assistant,
                threadlane_runtime::harness::UiMessageRole::System => MessageRole::System,
                threadlane_runtime::harness::UiMessageRole::Error => MessageRole::Error,
            },
            content: msg.content,
            tool_activities: msg
                .tool_activities
                .into_iter()
                .map(|act| {
                    let display_summary = tool_activity_display_summary(&act.summary);
                    ToolActivityInfo {
                        id: act.id,
                        category: act.category,
                        title: act.title,
                        display_summary,
                        detail: act.detail,
                        is_expanded: false,
                    }
                })
                .collect(),
            streaming: false,
            reasoning_content: msg.reasoning_content,
            reasoning_expanded: false,
        })
        .collect()
}

pub use threadlane_session::runtime_status_text;

pub fn coding_agent_options(
    work_dir: PathBuf,
    session_file: PathBuf,
    model: String,
    model_roles: threadlane_runtime::ModelRoles,
    browser: threadlane_protocol::browser::BrowserBridge,
) -> threadlane_coding_agent::CodingAgentOptions {
    let (api_key, account_id) = threadlane_coding_agent::credentials::provider_credentials(&model);
    let mut agent_config = threadlane_runtime::AgentConfig::default();
    agent_config.model_roles = model_roles;
    let subagent_settings = threadlane_runtime::subagent_settings::load(&work_dir);
    agent_config.subagent_model = subagent_settings.model;
    agent_config.subagent_reasoning_effort = subagent_settings.reasoning_effort;
    if agent_config.model_roles.fast.is_none() {
        agent_config.model_roles.fast = subagent_settings.fast_model;
    }
    agent_config.fast_reasoning_effort = subagent_settings.fast_reasoning_effort;
    agent_config.orchestrator_mode = subagent_settings.orchestrator_mode;

    threadlane_coding_agent::CodingAgentOptions {
        api_key,
        account_id,
        model,
        work_dir,
        session_file: Some(session_file),
        system_prompt: Default::default(),
        agent_config: Some(agent_config),
        coding_config: None,
        browser,
    }
}
