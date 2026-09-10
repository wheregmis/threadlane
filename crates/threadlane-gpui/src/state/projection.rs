use std::path::{Path, PathBuf};
use threadlane_session::AgentMessage;
use threadlane_session::harness::{JsonlStore, SessionStore};

use super::AppState;
use super::types::{
    ChatMessageInfo, MessageRole, SessionProjectionResult, SubagentActivityInfo,
    SubagentActivityStatus, ToolActivityInfo,
};
use crate::services::sessions::SessionRuntimeStatus;

pub(crate) fn extract_session_title(store: &impl SessionStore, fallback_id: &str) -> String {
    if let Some(name) = store.name() {
        if !name.trim().is_empty() {
            return name;
        }
    }
    let messages = {
        let active = store.active_branch_messages("main");
        if active.is_empty() {
            store.get_persisted_messages()
        } else {
            active
        }
    };

    for msg in &messages {
        match msg {
            AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    let first_line = trimmed.lines().next().unwrap_or(trimmed);
                    let mut char_count = 0;
                    let mut result = String::new();
                    for ch in first_line.chars() {
                        if char_count >= 40 {
                            result.push('…');
                            break;
                        }
                        result.push(ch);
                        char_count += 1;
                    }
                    return result;
                }
            }
            _ => {}
        }
    }
    fallback_id.to_string()
}

#[cfg(test)]
pub(crate) fn load_session_messages(session_file: &Path) -> Vec<ChatMessageInfo> {
    compute_session_messages(session_file).unwrap_or_default()
}

pub(crate) fn compute_session_messages(
    session_file: &Path,
) -> Result<Vec<ChatMessageInfo>, String> {
    use threadlane_session::harness::{TranscriptItem, read_transcript_page};

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
pub(crate) fn compute_full_session_projection(
    session_file: &Path,
) -> Result<SessionProjectionResult, String> {
    let store = JsonlStore::open_read_only(session_file).map_err(|error| error.to_string())?;
    let diagnostics = threadlane_session::harness::project_session_diagnostics(&store, "main")
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

pub(crate) fn project_subagents_from_store(store: &impl SessionStore) -> Vec<SubagentActivityInfo> {
    use threadlane_session::harness::{Record, SubagentLifecyclePhase};

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

pub(crate) fn tool_activity_summary(name: &str, arguments: &str) -> String {
    let display_name = name.replace('_', " ");
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return display_name;
    };
    let context = [
        "path",
        "file_path",
        "FilePath",
        "TargetFile",
        "command",
        "CommandLine",
        "query",
        "Query",
        "regex",
        "glob",
        "pattern",
        "Pattern",
        "prompt",
        "Prompt",
        "description",
        "Description",
    ]
    .iter()
    .find_map(|key| arguments.get(key).and_then(|value| value.as_str()));

    if let Some(value) = context {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
            let has_more_lines = trimmed.lines().nth(1).is_some();
            let mut summary_ctx = first_line.to_string();
            if has_more_lines && !summary_ctx.ends_with('…') && !summary_ctx.ends_with("...") {
                summary_ctx.push_str(" …");
            }
            return format!("{display_name} {summary_ctx}");
        }
    }
    display_name
}

pub(crate) fn tool_activity_display_summary(summary: &str) -> String {
    let first_line = summary.lines().next().unwrap_or(summary).trim();
    if summary.lines().nth(1).is_some()
        && !first_line.ends_with('…')
        && !first_line.ends_with("...")
    {
        format!("{first_line} …")
    } else {
        first_line.to_string()
    }
}

pub(crate) fn format_context_marker_tokens(tokens: usize) -> String {
    let formatted = crate::model_catalog::format_tokens(tokens.min(u32::MAX as usize) as u32);
    formatted.replace(".0k", "k").replace(".0M", "M")
}

pub(crate) fn project_agent_messages(agent_messages: Vec<AgentMessage>) -> Vec<ChatMessageInfo> {
    threadlane_session::harness::project_chat_messages(&agent_messages)
        .into_iter()
        .map(|msg| ChatMessageInfo {
            id: msg.id,
            role: match msg.role {
                threadlane_session::harness::UiMessageRole::User => MessageRole::User,
                threadlane_session::harness::UiMessageRole::Assistant => MessageRole::Assistant,
                threadlane_session::harness::UiMessageRole::System => MessageRole::System,
                threadlane_session::harness::UiMessageRole::Error => MessageRole::Error,
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

pub(crate) fn runtime_status_text(status: SessionRuntimeStatus) -> Option<String> {
    match status {
        SessionRuntimeStatus::Ready => None,
        SessionRuntimeStatus::Working => Some("Working…".into()),
        SessionRuntimeStatus::Interrupted => {
            Some("Turn interrupted · Safe replay checkpoints available".into())
        }
        SessionRuntimeStatus::Error(error) => Some(error),
    }
}

pub(crate) fn provider_credentials(model: &str) -> (String, Option<String>) {
    if threadlane_provider::router::is_antigravity_model(model) {
        return (
            threadlane_provider::antigravity_auth::load_antigravity_credentials()
                .map(|credentials| credentials.access_token)
                .unwrap_or_default(),
            None,
        );
    }
    if threadlane_provider::router::is_opencode_model(model) {
        return (
            threadlane_auth::opencode_auth::load_opencode_api_key().unwrap_or_default(),
            None,
        );
    }
    if let Some(api_key) =
        threadlane_auth::openai_auth::load_openai_api_key().filter(|key| !key.trim().is_empty())
    {
        return (api_key, None);
    }
    if let Some(credentials) = threadlane_auth::openai_auth::load_credentials()
        .filter(|credentials| threadlane_auth::openai_auth::is_own_source(&credentials.source))
    {
        return (credentials.access_token, credentials.account_id);
    }
    (std::env::var("OPENAI_API_KEY").unwrap_or_default(), None)
}

pub(crate) fn coding_agent_options(
    work_dir: PathBuf,
    session_file: PathBuf,
    model: String,
    model_roles: threadlane_session::ModelRoles,
    browser: threadlane_session::BrowserBridge,
) -> threadlane_session::CodingAgentOptions {
    let (api_key, account_id) = provider_credentials(&model);
    let mut agent_config = threadlane_session::AgentConfig::default();
    agent_config.model_roles = model_roles;
    let subagent_settings = crate::services::subagent_settings::load(&work_dir);
    agent_config.subagent_model = subagent_settings.model;
    agent_config.subagent_reasoning_effort = subagent_settings.reasoning_effort;
    if agent_config.model_roles.fast.is_none() {
        agent_config.model_roles.fast = subagent_settings.fast_model;
    }
    agent_config.fast_reasoning_effort = subagent_settings.fast_reasoning_effort;
    agent_config.orchestrator_mode = subagent_settings.orchestrator_mode;
    agent_config.needle_enabled = crate::services::settings::load_needle_enabled();

    threadlane_session::CodingAgentOptions {
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
