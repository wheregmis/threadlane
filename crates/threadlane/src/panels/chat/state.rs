//! Chat panel state: chat messages, tool call presentations, and streaming status.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use threadlane_agent::{AgentEvent, AgentMessage, SubagentRecoveryStatus};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MsgRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ToolStatus {
    Running,
    Done,
    Error,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolIcon {
    ReadFile,
    WriteFile,
    EditFile,
    ListDirectory,
    Terminal,
    Skill,
    Subagent,
    Generic,
}

#[derive(Clone, Debug)]
pub struct ToolPresentation {
    pub icon: ToolIcon,
    pub title: String,
    pub primary: String,
    pub metadata: String,
    pub arguments_detail: String,
    pub output_markdown: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct SubagentSessionData {
    #[serde(default)]
    pub run_id: Option<String>,
    pub task: String,
    pub agent: String,
    pub status: String,
    pub thinking: String,
    pub inner_tools: Vec<SubagentInnerToolData>,
    pub output: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct SubagentInnerToolData {
    pub name: String,
    pub target_preview: String,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentRailItem {
    pub key: Option<String>,
    pub agent: String,
    pub task: String,
    pub status: String,
    pub detail: String,
    pub parent_lane: Option<String>,
    pub token_usage: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarnessActivityStatus {
    Queued,
    Working,
    Recovering,
    Recovered,
    Retrying,
    Aborted,
    Cancelled,
    Faulted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessActivity {
    pub key: String,
    pub task: String,
    pub agent: String,
    pub status: HarnessActivityStatus,
    pub detail: String,
}

pub fn reduce_harness_activity(activities: &mut Vec<HarnessActivity>, activity: HarnessActivity) {
    if let Some(existing) = activities
        .iter_mut()
        .find(|existing| existing.key == activity.key)
    {
        if !matches!(
            existing.status,
            HarnessActivityStatus::Recovered
                | HarnessActivityStatus::Aborted
                | HarnessActivityStatus::Cancelled
                | HarnessActivityStatus::Faulted
        ) {
            *existing = activity;
        }
    } else {
        activities.push(activity);
    }
}

pub fn harness_activity_label(activity: &HarnessActivity) -> String {
    match activity.status {
        HarnessActivityStatus::Queued => "Delegated",
        HarnessActivityStatus::Working => "Working",
        HarnessActivityStatus::Recovering => "Recovering",
        HarnessActivityStatus::Recovered => "Recovered",
        HarnessActivityStatus::Retrying => "Retrying recovery",
        HarnessActivityStatus::Aborted => "Aborted · unsafe tool",
        HarnessActivityStatus::Cancelled => "Cancelled",
        HarnessActivityStatus::Faulted => "Harness fault",
    }
    .into()
}

pub fn harness_activity_detail(activity: &HarnessActivity) -> String {
    let detail = normalize_whitespace_bounded(&activity.detail, 240);
    if detail.is_empty() {
        harness_activity_label(activity)
    } else {
        detail
    }
}

pub fn merge_harness_activities(
    rail_items: &mut Vec<SubagentRailItem>,
    activities: &[HarnessActivity],
) {
    for activity in activities {
        let item = SubagentRailItem {
            key: Some(activity.key.clone()),
            agent: {
                let agent = normalize_whitespace_bounded(&activity.agent, 48);
                if agent.is_empty() {
                    "subagent".into()
                } else {
                    agent
                }
            },
            task: {
                let task = normalize_whitespace_bounded(&activity.task, 160);
                if task.is_empty() {
                    "Subagent task".into()
                } else {
                    task
                }
            },
            status: harness_activity_label(activity),
            detail: harness_activity_detail(activity),
            parent_lane: None,
            token_usage: None,
        };
        if let Some(existing) = rail_items
            .iter_mut()
            .find(|existing| existing.key.as_deref() == Some(&activity.key))
        {
            *existing = item;
        } else {
            rail_items.push(item);
        }
    }
}

pub fn reduce_harness_event(data: &mut ChatData, event: AgentEvent) {
    let activity = match event {
        AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent,
            task,
        } => HarnessActivity {
            key: subagent_activity_key(run_id, task_index),
            task,
            agent,
            status: HarnessActivityStatus::Queued,
            detail: "Queued".into(),
        },
        AgentEvent::SubagentStarted {
            run_id,
            task_index,
            journal_run_id,
        } => {
            let queued_key = subagent_activity_key(run_id, task_index);
            migrate_harness_activity_key(
                &mut data.harness_activities,
                &queued_key,
                &journal_run_id,
            );
            let key = journal_run_id;
            let (task, agent) = harness_activity_identity(&data.harness_activities, &key);
            HarnessActivity {
                key,
                task,
                agent,
                status: HarnessActivityStatus::Working,
                detail: "Working".into(),
            }
        }
        AgentEvent::SubagentFinished {
            run_id,
            task_index,
            journal_run_id,
            succeeded,
            error,
        } => {
            let queued_key = subagent_activity_key(run_id, task_index);
            migrate_harness_activity_key(
                &mut data.harness_activities,
                &queued_key,
                &journal_run_id,
            );
            let key = journal_run_id;
            let (task, agent) = harness_activity_identity(&data.harness_activities, &key);
            let (status, detail) = subagent_finished_status(succeeded, error.as_deref());
            HarnessActivity {
                key,
                task,
                agent,
                status,
                detail,
            }
        }
        AgentEvent::SubagentRecovery {
            run_id,
            status,
            detail,
        } => {
            let (task, agent) = harness_activity_identity(&data.harness_activities, &run_id);
            let (status, fallback) = match status {
                SubagentRecoveryStatus::Started => (
                    HarnessActivityStatus::Recovering,
                    "Recovering interrupted task",
                ),
                SubagentRecoveryStatus::Recovered => {
                    (HarnessActivityStatus::Recovered, "Recovered prior work")
                }
                SubagentRecoveryStatus::Retrying => {
                    (HarnessActivityStatus::Retrying, "Recovery needs retry")
                }
                SubagentRecoveryStatus::Aborted => {
                    (HarnessActivityStatus::Aborted, "Recovery was aborted")
                }
            };
            HarnessActivity {
                key: run_id,
                task,
                agent,
                status,
                detail: detail.unwrap_or_else(|| fallback.into()),
            }
        }
        _ => return,
    };
    reduce_harness_activity(&mut data.harness_activities, activity);
    data.revision = data.revision.wrapping_add(1);
}

fn subagent_activity_key(run_id: u64, task_index: usize) -> String {
    format!("subagent-{run_id}:{task_index}")
}

fn migrate_harness_activity_key(activities: &mut [HarnessActivity], from: &str, to: &str) {
    if from != to {
        if let Some(activity) = activities.iter_mut().find(|activity| activity.key == from) {
            activity.key = to.into();
        }
    }
}

fn harness_activity_identity(activities: &[HarnessActivity], key: &str) -> (String, String) {
    activities
        .iter()
        .find(|activity| activity.key == key)
        .map(|activity| (activity.task.clone(), activity.agent.clone()))
        .unwrap_or_else(|| ("Subagent task".into(), "subagent".into()))
}

fn subagent_finished_status(
    succeeded: bool,
    error: Option<&str>,
) -> (HarnessActivityStatus, String) {
    if succeeded {
        return (HarnessActivityStatus::Recovered, "Completed".into());
    }
    let error = error.unwrap_or("Subagent failed");
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("unsafe") {
        (HarnessActivityStatus::Aborted, "Unsafe interruption".into())
    } else if normalized.contains("cancel") {
        (HarnessActivityStatus::Cancelled, "Cancelled".into())
    } else {
        (
            HarnessActivityStatus::Retrying,
            normalize_whitespace_bounded(error, 160),
        )
    }
}

#[cfg(test)]
pub fn subagent_rail_items(
    arguments: &str,
    output: &str,
    status: ToolStatus,
    messages: &[ChatMessage],
    child_run: Option<u64>,
) -> Vec<SubagentRailItem> {
    subagent_rail_items_with_harness(arguments, output, status, messages, child_run)
}

pub fn subagent_rail_items_with_harness(
    arguments: &str,
    output: &str,
    status: ToolStatus,
    messages: &[ChatMessage],
    child_run: Option<u64>,
) -> Vec<SubagentRailItem> {
    let child_run = (status == ToolStatus::Running)
        .then_some(child_run)
        .flatten();
    if let Ok(sessions) = serde_json::from_str::<Vec<SubagentSessionData>>(output) {
        if !sessions.is_empty() {
            return sessions
                .into_iter()
                .enumerate()
                .map(|(index, session)| {
                    let detail = if child_run.is_some() {
                        subagent_task_activity_detail(messages, child_run, index)
                    } else {
                        subagent_session_detail(&session)
                    };
                    SubagentRailItem {
                        key: session.run_id,
                        agent: session.agent,
                        task: normalize_whitespace_bounded(&session.task, 160),
                        status: session.status,
                        detail,
                        parent_lane: None,
                        token_usage: None,
                    }
                })
                .collect();
        }
    }

    let parsed = serde_json::from_str::<serde_json::Value>(arguments).ok();
    let parallel = parsed
        .as_ref()
        .and_then(|value| value.get("parallel"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    parsed
        .as_ref()
        .and_then(|value| value.get("tasks"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, task)| {
            let agent = task
                .get("agent")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("subagent")
                .to_string();
            let task_text = task
                .get("task")
                .and_then(serde_json::Value::as_str)
                .map(|task| normalize_whitespace_bounded(task, 160))
                .unwrap_or_default();
            SubagentRailItem {
                key: None,
                agent,
                task: task_text,
                status: match status {
                    ToolStatus::Running if parallel || index == 0 => "Working",
                    ToolStatus::Running => "Queued",
                    ToolStatus::Done => "Done",
                    ToolStatus::Error => "Failed",
                    ToolStatus::Cancelled => "Stopped",
                }
                .to_string(),
                detail: subagent_task_activity_detail(messages, child_run, index),
                parent_lane: None,
                token_usage: None,
            }
        })
        .collect()
}

pub fn is_subagent_child_tool(message: &ChatMessage) -> bool {
    matches!(message, ChatMessage::Tool { id, .. } if subagent_child_tool_tag(id).is_some())
}

pub fn owned_subagent_child_runs(messages: &[ChatMessage]) -> HashMap<usize, u64> {
    let mut runs = HashMap::new();
    let mut parent_index = None;

    for (index, message) in messages.iter().enumerate() {
        if is_subagent_parent_tool(message) {
            parent_index = Some(index);
        } else if let Some(parent) = parent_index {
            if let ChatMessage::Tool { id, .. } = message {
                if let Some(tag) = subagent_child_tool_tag(id) {
                    runs.entry(parent).or_insert(tag.run_id);
                    parent_index = None;
                }
            }
        }
    }

    runs
}

pub fn is_owned_subagent_child_tool(message: &ChatMessage, owned_runs: &[u64]) -> bool {
    is_subagent_child_tool(message)
        && matches!(message, ChatMessage::Tool { id, .. } if matches!(subagent_child_tool_tag(id), Some(tag) if owned_runs.contains(&tag.run_id)))
}

fn is_subagent_parent_tool(message: &ChatMessage) -> bool {
    matches!(message, ChatMessage::Tool { name, presentation, .. } if name == "subagent" || presentation.icon == ToolIcon::Subagent)
}

fn subagent_task_activity_detail(
    messages: &[ChatMessage],
    run_id: Option<u64>,
    task_index: usize,
) -> String {
    let Some(run_id) = run_id else {
        return String::new();
    };
    messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::Tool {
                id,
                name,
                presentation,
                status,
                ..
            } if matches!(subagent_child_tool_tag(id), Some(tag) if tag.run_id == run_id && tag.task_index == task_index) => {
                let target = if presentation.primary.is_empty() {
                    String::new()
                } else {
                    format!(" `{}`", presentation.primary)
                };
                Some(format!("- **{}**{} · {}", name, target, subagent_tool_status(*status)))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SubagentChildToolTag {
    run_id: u64,
    task_index: usize,
}

fn subagent_child_tool_tag(tool_call_id: &str) -> Option<SubagentChildToolTag> {
    let tagged = tool_call_id.strip_prefix("subagent-")?;
    let (run_id, tagged) = tagged.split_once(':')?;
    let (task_index, tool_call_id) = tagged.split_once(':')?;
    if run_id.is_empty()
        || task_index.is_empty()
        || tool_call_id.is_empty()
        || !run_id.bytes().all(|byte| byte.is_ascii_digit())
        || !task_index.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(SubagentChildToolTag {
        run_id: run_id.parse().ok()?,
        task_index: task_index.parse().ok()?,
    })
}

fn subagent_tool_status(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Running => "Working",
        ToolStatus::Done => "Done",
        ToolStatus::Error => "Failed",
        ToolStatus::Cancelled => "Stopped",
    }
}

fn subagent_session_detail(session: &SubagentSessionData) -> String {
    let mut sections = Vec::new();
    if !session.thinking.trim().is_empty() {
        sections.push(format!("**Thinking**\n\n{}", session.thinking));
    }
    if !session.inner_tools.is_empty() {
        let mut activity = "**Activity**".to_string();
        for tool in &session.inner_tools {
            let status = if tool.is_error { "✗" } else { "✓" };
            activity.push_str(&format!(
                "\n- {status} `{}` · {}",
                tool.name, tool.target_preview
            ));
        }
        sections.push(activity);
    }
    if !session.output.trim().is_empty() {
        sections.push(format!("**Report**\n\n{}", session.output));
    }
    sections.join("\n\n")
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum ChatMessage {
    Text {
        role: MsgRole,
        text: String,
    },
    Thinking {
        text: String,
    },
    Tool {
        id: String,
        name: String,
        arguments: String,
        output: String,
        status: ToolStatus,
        presentation: ToolPresentation,
        result_preview: String,
        result_metadata: String,
        started_at: Instant,
    },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StreamingKind {
    Assistant,
    Thinking,
}

#[derive(Clone, Debug, Default)]
pub struct ChatData {
    pub messages: Vec<ChatMessage>,
    pub streaming_text: String,
    pub streaming_kind: Option<StreamingKind>,
    pub harness_activities: Vec<HarnessActivity>,
    pub revision: u64,
}

impl ChatData {
    pub fn push_chat(&mut self, role: MsgRole, text: impl Into<String>) {
        self.messages.push(ChatMessage::Text {
            role,
            text: text.into(),
        });
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn push_thinking(&mut self, text: String) {
        push_thinking_locked(self, text);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn push_stream_delta(&mut self, kind: StreamingKind, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.streaming_kind != Some(kind) {
            flush_streaming_locked(self);
            self.streaming_kind = Some(kind);
        }
        self.streaming_text.push_str(delta);
    }

    pub fn flush_streaming(&mut self) {
        flush_streaming_locked(self);
    }

    pub fn flush_tool_call_preamble(&mut self) {
        let text = std::mem::take(&mut self.streaming_text);
        self.streaming_kind = None;
        self.push_thinking(text);
    }

    /// Aborting the generation prevents normal stream and tool-end events, so
    /// commit any partial stream and explicitly finalize running tool rows.
    pub fn mark_generation_stopped(&mut self) {
        self.flush_streaming();
        for activity in &mut self.harness_activities {
            if matches!(
                activity.status,
                HarnessActivityStatus::Queued
                    | HarnessActivityStatus::Working
                    | HarnessActivityStatus::Recovering
                    | HarnessActivityStatus::Retrying
            ) {
                activity.status = HarnessActivityStatus::Cancelled;
                activity.detail = "Cancelled".into();
            }
        }
        for message in &mut self.messages {
            let ChatMessage::Tool {
                name,
                output,
                status,
                result_preview,
                result_metadata,
                started_at,
                ..
            } = message
            else {
                continue;
            };
            if *status != ToolStatus::Running {
                continue;
            }

            *status = ToolStatus::Cancelled;
            *result_preview = tool_result_preview(output, 800);
            *result_metadata =
                result_metadata_for_tool(name, output, ToolStatus::Cancelled, started_at.elapsed());
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn push_tool(&mut self, id: String, name: String, arguments: String) {
        self.flush_streaming();
        let presentation = tool_presentation(&name, &arguments);
        if let Some(ChatMessage::Tool {
            name: existing_name,
            arguments: existing_arguments,
            status,
            presentation: existing_presentation,
            output,
            result_preview,
            result_metadata,
            started_at,
            ..
        }) = self.messages.iter_mut().rev().find(|message| {
            matches!(message, ChatMessage::Tool { id: existing_id, .. } if existing_id == &id)
        }) {
            *existing_name = name;
            *existing_arguments = arguments;
            *existing_presentation = presentation;
            *output = String::new();
            *result_preview = String::new();
            result_metadata.clear();
            *status = ToolStatus::Running;
            *started_at = Instant::now();
            self.revision = self.revision.wrapping_add(1);
            return;
        }
        self.messages.push(ChatMessage::Tool {
            id,
            name,
            arguments,
            output: String::new(),
            status: ToolStatus::Running,
            presentation,
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        });
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn update_tool(&mut self, id: &str, output: String, status: Option<ToolStatus>) {
        if let Some(ChatMessage::Tool {
            name,
            output: existing_output,
            status: existing_status,
            result_preview,
            result_metadata,
            started_at,
            ..
        }) = self.messages.iter_mut().rev().find(
            |message| matches!(message, ChatMessage::Tool { id: existing_id, .. } if existing_id == id),
        ) {
            *existing_output = output;
            *result_preview = tool_result_preview(existing_output, 800);
            *result_metadata = result_metadata_for_tool(
                name,
                existing_output,
                status.unwrap_or(*existing_status),
                started_at.elapsed(),
            );
            if let Some(status) = status {
                *existing_status = status;
            }
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn replace_from_agent_messages(&mut self, messages: &[AgentMessage]) {
        self.messages.clear();
        self.streaming_text.clear();
        self.streaming_kind = None;
        self.revision = self.revision.wrapping_add(1);
        for msg in messages {
            match msg {
                AgentMessage::User { content } => self.push_chat(MsgRole::User, content.clone()),
                AgentMessage::UserWithImages { content, images } => {
                    let names = images
                        .iter()
                        .map(|image| image.display_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let text = if content.trim().is_empty() {
                        format!("Attached: {names}")
                    } else {
                        format!("{content}\n\nAttached: {names}")
                    };
                    self.push_chat(MsgRole::User, text);
                }
                AgentMessage::Assistant {
                    content,
                    tool_calls,
                    ..
                } => {
                    if let Some(text) = content {
                        if !text.is_empty() {
                            if tool_calls.is_some() {
                                self.push_thinking(text.clone());
                            } else {
                                self.push_chat(MsgRole::Assistant, text.clone());
                            }
                        }
                    }
                    if let Some(tool_calls) = tool_calls {
                        for call in tool_calls {
                            let presentation =
                                tool_presentation(&call.function.name, &call.function.arguments);
                            self.messages.push(ChatMessage::Tool {
                                id: call.id.clone(),
                                name: call.function.name.clone(),
                                arguments: call.function.arguments.clone(),
                                output: String::new(),
                                status: ToolStatus::Running,
                                presentation,
                                result_preview: String::new(),
                                result_metadata: String::new(),
                                started_at: Instant::now(),
                            });
                        }
                    }
                }
                AgentMessage::Tool {
                    tool_call_id,
                    name,
                    content,
                    is_error,
                    ..
                } => {
                    let status = if *is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Done
                    };
                    self.update_tool(tool_call_id, content.clone(), Some(status));
                    if !self.messages.iter().any(|message| matches!(message, ChatMessage::Tool { id, .. } if id == tool_call_id)) {
                        let presentation = tool_presentation(name, "");
                        self.messages.push(ChatMessage::Tool {
                            id: tool_call_id.clone(), name: name.clone(), arguments: String::new(), output: content.clone(), status, presentation,
                            result_preview: tool_result_preview(content, 800), result_metadata: result_metadata_for_tool(name, content, status, Duration::ZERO), started_at: Instant::now(),
                        });
                    }
                }
                AgentMessage::Custom {
                    custom_type,
                    payload,
                } if custom_type == "thinking" => {
                    if let Some(text) = payload.get("text").and_then(serde_json::Value::as_str) {
                        self.push_thinking(text.to_string());
                    }
                }
                AgentMessage::System { .. } | AgentMessage::Custom { .. } => {}
            }
        }
    }
}

fn push_thinking_locked(data: &mut ChatData, text: String) {
    let incoming = text.trim();
    if incoming.is_empty() {
        return;
    }
    if let Some(ChatMessage::Thinking { text: existing }) = data.messages.last_mut() {
        if existing.trim() == incoming {
            return;
        }
        if !existing.is_empty() {
            existing.push_str("\n\n");
        }
        existing.push_str(&text);
    } else {
        data.messages.push(ChatMessage::Thinking { text });
    }
}

fn flush_streaming_locked(data: &mut ChatData) {
    let text = std::mem::take(&mut data.streaming_text);
    let kind = data.streaming_kind.take();
    if text.trim().is_empty() {
        return;
    }
    match kind {
        Some(StreamingKind::Thinking) => push_thinking_locked(data, text),
        _ => data.messages.push(ChatMessage::Text {
            role: MsgRole::Assistant,
            text,
        }),
    }
}

impl ToolIcon {
    pub fn from_name(name: &str) -> Self {
        match name {
            "read_file" => ToolIcon::ReadFile,
            "write_file" => ToolIcon::WriteFile,
            "edit_file" => ToolIcon::EditFile,
            "list_dir" | "list_directory" => ToolIcon::ListDirectory,
            "run_command" => ToolIcon::Terminal,
            "load_skill" => ToolIcon::Skill,
            "subagent" => ToolIcon::Subagent,
            _ => ToolIcon::Generic,
        }
    }
}

pub fn tool_icon(name: &str) -> ToolIcon {
    ToolIcon::from_name(name)
}

pub fn tool_title(name: &str) -> String {
    match name {
        "run_command" => "Run command".into(),
        "read_file" => "Read file".into(),
        "write_file" => "Write file".into(),
        "edit_file" => "Edit file".into(),
        "list_dir" | "list_directory" => "List directory".into(),
        "grep" => "Search".into(),
        "find_path" => "Find files".into(),
        "load_skill" => "Load skill".into(),
        "subagent" => "Delegate".into(),
        _ => name.replace('_', " "),
    }
}

pub fn tool_presentation(name: &str, arguments: &str) -> ToolPresentation {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments).ok();
    let args = parsed.as_ref();
    let get_str = |key: &str| {
        args.and_then(|value| value.get(key))
            .and_then(serde_json::Value::as_str)
    };
    let path = get_str("path").map(compact_path).unwrap_or_default();
    let pretty_arguments = parsed
        .as_ref()
        .and_then(|value| serde_json::to_string_pretty(value).ok())
        .unwrap_or_else(|| arguments.to_string());

    let (primary, metadata, arguments_detail, output_markdown) = match name {
        "run_command" => (
            compact_command(get_str("command").unwrap_or(arguments)),
            String::new(),
            pretty_arguments.clone(),
            false,
        ),
        "read_file" => {
            let start = args
                .and_then(|value| value.get("start_line"))
                .and_then(serde_json::Value::as_u64);
            let end = args
                .and_then(|value| value.get("end_line"))
                .and_then(serde_json::Value::as_u64);
            let range = match (start, end) {
                (Some(start), Some(end)) => format!("lines {start}–{end}"),
                (Some(start), None) => format!("from line {start}"),
                _ => String::new(),
            };
            (path.clone(), range, pretty_arguments.clone(), false)
        }
        "write_file" => {
            let content = get_str("content").unwrap_or_default();
            (
                path.clone(),
                text_size_label(content),
                pretty_arguments.clone(),
                false,
            )
        }
        "edit_file" => {
            let (removed, added) = args
                .and_then(|value| value.get("edits"))
                .and_then(serde_json::Value::as_array)
                .map(|edits| {
                    edits.iter().fold((0, 0), |(removed, added), edit| {
                        let old = edit
                            .get("old_text")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let new = edit
                            .get("new_text")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        (removed + line_count(old), added + line_count(new))
                    })
                })
                .unwrap_or_else(|| {
                    let old = get_str("target").unwrap_or_default();
                    let new = get_str("replacement").unwrap_or_default();
                    (line_count(old), line_count(new))
                });
            (
                path.clone(),
                format!("+{added} −{removed} lines"),
                pretty_arguments.clone(),
                false,
            )
        }
        "list_dir" | "list_directory" => (
            if path.is_empty() {
                ".".into()
            } else {
                path.clone()
            },
            String::new(),
            pretty_arguments.clone(),
            false,
        ),
        "grep" => (
            truncate_chars(get_str("regex").unwrap_or_default(), 96),
            get_str("include_pattern").unwrap_or("project").to_string(),
            pretty_arguments.clone(),
            false,
        ),
        "find_path" => (
            truncate_chars(get_str("glob").unwrap_or_default(), 96),
            "project files".into(),
            pretty_arguments.clone(),
            false,
        ),
        "load_skill" => {
            let skill_id = get_str("name").unwrap_or_default();
            (
                truncate_chars(skill_id, 96),
                "skill instructions".into(),
                format!("Skill ID: {}", truncate_chars(skill_id, 128)),
                true,
            )
        }
        "subagent" => {
            let (primary, metadata, detail) = subagent_presentation(args, arguments);
            (primary, metadata, detail, true)
        }
        _ => (
            truncate_chars(arguments, 120),
            String::new(),
            pretty_arguments,
            false,
        ),
    };

    ToolPresentation {
        icon: tool_icon(name),
        title: tool_title(name),
        primary,
        metadata,
        arguments_detail,
        output_markdown,
    }
}

fn subagent_presentation(
    args: Option<&serde_json::Value>,
    fallback: &str,
) -> (String, String, String) {
    const MAX_VISIBLE_TASKS: usize = 8;
    const MAX_VISIBLE_AGENT_CHARS: usize = 128;
    const MAX_VISIBLE_TASK_CHARS: usize = 240;

    let Some(tasks) = args
        .and_then(|value| value.get("tasks"))
        .and_then(serde_json::Value::as_array)
    else {
        return (
            truncate_chars(fallback, 120),
            String::new(),
            truncate_chars(fallback, 2_000),
        );
    };
    let visible_tasks: Vec<_> = tasks.iter().take(MAX_VISIBLE_TASKS).collect();
    let parallel = args
        .and_then(|value| value.get("parallel"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mode = if parallel { "parallel" } else { "sequential" };
    let agents: Vec<String> = visible_tasks
        .iter()
        .filter_map(|task| task.get("agent").and_then(serde_json::Value::as_str))
        .map(|agent| truncate_chars(agent, MAX_VISIBLE_AGENT_CHARS))
        .collect();
    let task_summary = |task: &serde_json::Value| {
        task.get("task")
            .and_then(serde_json::Value::as_str)
            .map(|task| normalize_whitespace_bounded(task, MAX_VISIBLE_TASK_CHARS))
            .unwrap_or_default()
    };

    let primary = if tasks.len() == 1 {
        let agent = agents.first().map(String::as_str).unwrap_or("subagent");
        let summary = task_summary(visible_tasks[0]);
        format!(
            "{} · {}",
            truncate_chars(agent, 64),
            truncate_chars(&summary, 84)
        )
    } else {
        format!("{} tasks", tasks.len())
    };
    let metadata = {
        let agent_list = truncate_chars(&agents.join(", "), 160);
        if agent_list.is_empty() {
            mode.to_string()
        } else {
            format!("{mode} · {agent_list}")
        }
    };
    let mut detail = format!(
        "### Subagent Delegation ({})\n",
        if parallel {
            "Parallel Execution"
        } else {
            "Sequential Chain"
        }
    );
    for (index, task) in visible_tasks.iter().enumerate() {
        let agent = task
            .get("agent")
            .and_then(serde_json::Value::as_str)
            .map(|agent| truncate_chars(agent, MAX_VISIBLE_AGENT_CHARS))
            .unwrap_or_else(|| "subagent".into());
        let summary = task_summary(task);
        detail.push_str(&format!(
            "\n**Subagent Session {} (`{}`)**\n- **Task**: {}\n",
            index + 1,
            agent,
            summary
        ));
        if let Some(inst) = task.get("instructions").and_then(serde_json::Value::as_str) {
            detail.push_str(&format!(
                "- **Instructions**: {}\n",
                normalize_whitespace_bounded(inst, 180)
            ));
        }
        if let Some(tools) = task.get("tools").and_then(serde_json::Value::as_array) {
            let tool_names: Vec<_> = tools.iter().filter_map(serde_json::Value::as_str).collect();
            if !tool_names.is_empty() {
                detail.push_str(&format!("- **Tools**: `{}`\n", tool_names.join("`, `")));
            }
        }
    }
    if tasks.len() > visible_tasks.len() {
        detail.push_str(&format!(
            "\n… {} additional subagent tasks omitted",
            tasks.len() - visible_tasks.len()
        ));
    }
    (primary, metadata, detail)
}

pub(crate) fn normalize_whitespace_bounded(text: &str, max_chars: usize) -> String {
    text.chars()
        .take(max_chars)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn collapsed_thinking_preview(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let source_budget = max_chars.saturating_mul(4).saturating_add(64);
    let mut plain = String::with_capacity(source_budget.min(text.len()));
    let mut source_chars = 0;
    for line in text.lines() {
        let line = strip_markdown_line_prefix(line);
        if line.is_empty() {
            continue;
        }
        if !plain.is_empty() {
            plain.push(' ');
        }
        for character in line.chars() {
            if source_chars >= source_budget {
                break;
            }
            plain.push(character);
            source_chars += 1;
        }
        if source_chars >= source_budget {
            break;
        }
    }

    let remove_backticks = plain.matches('`').count() >= 2;
    let plain = plain.replace("**", "").replace("~~", "");
    let chars: Vec<_> = plain.chars().collect();
    let mut plain_text = String::with_capacity(plain.len());
    for (index, character) in chars.iter().copied().enumerate() {
        let remove = match character {
            '`' => remove_backticks,
            '_' => {
                let previous_is_alphanumeric = index
                    .checked_sub(1)
                    .and_then(|previous| chars.get(previous))
                    .is_some_and(|character| character.is_alphanumeric());
                let next_is_alphanumeric = chars
                    .get(index + 1)
                    .is_some_and(|character| character.is_alphanumeric());
                !(previous_is_alphanumeric && next_is_alphanumeric)
            }
            _ => false,
        };
        if !remove {
            plain_text.push(character);
        }
    }
    let normalized = plain_text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&normalized, max_chars)
}

fn strip_markdown_line_prefix(line: &str) -> &str {
    let mut line = line.trim();
    loop {
        let previous = line;
        if let Some(rest) = line.strip_prefix('>') {
            line = rest.trim_start();
        } else {
            let heading_end = line
                .chars()
                .take_while(|character| *character == '#')
                .count();
            if heading_end > 0
                && line
                    .chars()
                    .nth(heading_end)
                    .is_some_and(char::is_whitespace)
            {
                line = line[heading_end..].trim_start();
            } else if let Some(rest) = line
                .strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .or_else(|| line.strip_prefix("+ "))
            {
                line = rest.trim_start();
            } else if let Some(marker_end) = ordered_list_marker_end(line) {
                line = line[marker_end..].trim_start();
            }
        }
        if line == previous {
            return line;
        }
    }
}

fn ordered_list_marker_end(line: &str) -> Option<usize> {
    let digits_end = line
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit())
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    let rest = &line[digits_end..];
    let marker = rest.chars().next()?;
    if !matches!(marker, '.' | ')') {
        return None;
    }
    let marker_end = digits_end + marker.len_utf8();
    rest[marker.len_utf8()..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then_some(marker_end)
}

fn compact_command(command: &str) -> String {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&normalized, 48)
}

fn compact_path(path: &str) -> String {
    let candidate = Path::new(path);
    if let Ok(work_dir) = std::env::current_dir() {
        if let Ok(relative) = candidate.strip_prefix(&work_dir) {
            return relative.display().to_string();
        }
    }
    if candidate.is_absolute() {
        let parts: Vec<_> = candidate.components().collect();
        if parts.len() > 4 {
            return format!(
                "…/{}/{}/{}",
                parts[parts.len() - 3].as_os_str().to_string_lossy(),
                parts[parts.len() - 2].as_os_str().to_string_lossy(),
                parts[parts.len() - 1].as_os_str().to_string_lossy()
            );
        }
    }
    path.to_string()
}

fn line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

fn text_size_label(text: &str) -> String {
    format!(
        "{} lines · {}",
        line_count(text),
        byte_size_label(text.len())
    )
}

fn byte_size_label(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn tool_result_preview(output: &str, max_chars: usize) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "(empty output)".into();
    }
    collapsed_thinking_preview(trimmed, max_chars)
}

pub fn tool_result_detail(output: &str, max_chars: usize) -> String {
    const MAX_DETAIL_LINES: usize = 20;

    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "(empty output)".into();
    }

    let lines: Vec<_> = trimmed.lines().collect();
    let original_fits = lines.len() <= MAX_DETAIL_LINES && trimmed.chars().count() <= max_chars;
    if original_fits {
        return trimmed.to_string();
    }

    let content_line_limit = MAX_DETAIL_LINES - 1;
    let visible_lines = lines.len().min(content_line_limit);
    let body = lines[..visible_lines].join("\n");
    let omitted_lines = lines.len().saturating_sub(visible_lines);
    let notice = match (trimmed.chars().count() > max_chars, omitted_lines) {
        (true, 0) => "… output truncated".to_string(),
        (true, 1) => "… output truncated; 1 line omitted".to_string(),
        (true, count) => format!("… output truncated; {count} lines omitted"),
        (false, 1) => "… 1 line omitted".to_string(),
        (false, count) => format!("… {count} lines omitted"),
    };
    if notice.chars().count() >= max_chars {
        return truncate_chars(&notice, max_chars);
    }

    let body_budget = max_chars - notice.chars().count() - 1;
    let body = truncate_chars(&body, body_budget);
    format!("{body}\n{notice}")
}

#[cfg(test)]
fn result_metadata_for(output: &str, status: ToolStatus, duration: Duration) -> String {
    result_metadata_for_tool("", output, status, duration)
}

fn result_metadata_for_tool(
    name: &str,
    output: &str,
    status: ToolStatus,
    duration: Duration,
) -> String {
    let secs = duration.as_secs_f32();
    let time_label = if secs < 0.1 {
        "<0.1s".to_string()
    } else {
        format!("{secs:.1}s")
    };
    match status {
        ToolStatus::Running => String::new(),
        ToolStatus::Error => format!("Failed · {time_label}"),
        ToolStatus::Cancelled => format!("Stopped · {time_label}"),
        ToolStatus::Done if name == "load_skill" => format!("Loaded · {time_label}"),
        ToolStatus::Done if name == "subagent" => format!("Completed · {time_label}"),
        ToolStatus::Done => {
            let lines = line_count(output);
            let line_label = if lines == 1 { "line" } else { "lines" };
            let bytes = output.len();
            format!(
                "{lines} {line_label} · {} · {time_label}",
                byte_size_label(bytes)
            )
        }
    }
}

pub use crate::path_utils::truncate_chars;

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_agent::AgentEvent;

    fn harness_activity(key: &str, status: HarnessActivityStatus) -> HarnessActivity {
        HarnessActivity {
            key: key.into(),
            task: format!("{key} task"),
            agent: "scout".into(),
            status,
            detail: format!("{key} detail"),
        }
    }

    #[test]
    fn harness_activity_presentation_uses_concise_lifecycle_copy() {
        let cases = [
            (HarnessActivityStatus::Queued, "Delegated"),
            (HarnessActivityStatus::Working, "Working"),
            (HarnessActivityStatus::Recovering, "Recovering"),
            (HarnessActivityStatus::Recovered, "Recovered"),
            (HarnessActivityStatus::Retrying, "Retrying recovery"),
            (HarnessActivityStatus::Aborted, "Aborted · unsafe tool"),
            (HarnessActivityStatus::Cancelled, "Cancelled"),
            (HarnessActivityStatus::Faulted, "Harness fault"),
        ];

        for (status, expected) in cases {
            let activity = HarnessActivity {
                detail: "  Recovery\nreason  ".into(),
                ..harness_activity("lane-a", status)
            };
            assert_eq!(harness_activity_label(&activity), expected);
            assert_eq!(harness_activity_detail(&activity), "Recovery reason");
        }
    }

    #[test]
    fn merge_harness_activities_replaces_a_durable_rail_item() {
        let mut rail_items = Vec::new();
        merge_harness_activities(
            &mut rail_items,
            &[harness_activity(
                "lane-a",
                HarnessActivityStatus::Recovering,
            )],
        );
        merge_harness_activities(
            &mut rail_items,
            &[HarnessActivity {
                detail: "Recovered prior work".into(),
                ..harness_activity("lane-a", HarnessActivityStatus::Recovered)
            }],
        );

        assert_eq!(rail_items.len(), 1);
        assert_eq!(rail_items[0].key.as_deref(), Some("lane-a"));
        assert_eq!(rail_items[0].status, "Recovered");
        assert_eq!(rail_items[0].detail, "Recovered prior work");
    }

    #[test]
    fn harness_activity_replaces_queued_with_working_then_recovered() {
        let mut activities = ChatData::default().harness_activities;
        reduce_harness_activity(
            &mut activities,
            harness_activity("lane-a", HarnessActivityStatus::Queued),
        );

        reduce_harness_activity(
            &mut activities,
            harness_activity("lane-a", HarnessActivityStatus::Working),
        );
        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].status, HarnessActivityStatus::Working);
        reduce_harness_activity(
            &mut activities,
            harness_activity("lane-a", HarnessActivityStatus::Recovered),
        );

        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].key, "lane-a");
        assert_eq!(activities[0].status, HarnessActivityStatus::Recovered);
    }

    #[test]
    fn harness_activity_replaces_retrying_with_aborted() {
        let mut activities = vec![harness_activity("lane-a", HarnessActivityStatus::Retrying)];

        reduce_harness_activity(
            &mut activities,
            harness_activity("lane-a", HarnessActivityStatus::Aborted),
        );

        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].status, HarnessActivityStatus::Aborted);
    }

    #[test]
    fn harness_activity_replaces_duplicate_key_without_reordering_lanes() {
        let mut activities = vec![
            harness_activity("lane-a", HarnessActivityStatus::Queued),
            harness_activity("lane-b", HarnessActivityStatus::Working),
        ];

        reduce_harness_activity(
            &mut activities,
            HarnessActivity {
                detail: "Retrying recovery".into(),
                ..harness_activity("lane-a", HarnessActivityStatus::Retrying)
            },
        );

        assert_eq!(activities.len(), 2);
        assert_eq!(activities[0].key, "lane-a");
        assert_eq!(activities[0].status, HarnessActivityStatus::Retrying);
        assert_eq!(activities[0].detail, "Retrying recovery");
        assert_eq!(activities[1].key, "lane-b");
    }

    #[test]
    fn harness_activity_ignores_delayed_updates_after_terminal_statuses() {
        for terminal in [
            HarnessActivityStatus::Recovered,
            HarnessActivityStatus::Aborted,
            HarnessActivityStatus::Cancelled,
        ] {
            let mut activities = vec![
                harness_activity("lane-a", terminal),
                harness_activity("lane-b", HarnessActivityStatus::Working),
            ];

            reduce_harness_activity(
                &mut activities,
                harness_activity("lane-a", HarnessActivityStatus::Recovering),
            );

            assert_eq!(activities.len(), 2);
            assert_eq!(activities[0].key, "lane-a");
            assert_eq!(activities[0].status, terminal);
            assert_eq!(activities[1].key, "lane-b");
        }
    }

    #[test]
    fn harness_activity_event_keeps_provider_failures_retryable() {
        let mut data = ChatData::default();
        reduce_harness_event(
            &mut data,
            AgentEvent::SubagentQueued {
                run_id: 4,
                task_index: 1,
                agent: "reviewer".into(),
                task: "Review the patch".into(),
            },
        );
        reduce_harness_event(
            &mut data,
            AgentEvent::SubagentFinished {
                run_id: 4,
                task_index: 1,
                journal_run_id: "subagent-run-4".into(),
                succeeded: false,
                error: Some("provider unavailable".into()),
            },
        );

        assert_eq!(data.harness_activities.len(), 1);
        assert_eq!(
            data.harness_activities[0].status,
            HarnessActivityStatus::Retrying
        );
        assert_eq!(data.harness_activities[0].detail, "provider unavailable");
    }

    #[test]
    fn harness_activity_event_distinguishes_cancelled_and_unsafe_failures() {
        let mut data = ChatData::default();
        for (task_index, error) in [(0, "parent is cancelling"), (1, "unsafe tool interruption")] {
            reduce_harness_event(
                &mut data,
                AgentEvent::SubagentQueued {
                    run_id: 5,
                    task_index,
                    agent: "scout".into(),
                    task: "Inspect the repository".into(),
                },
            );
            reduce_harness_event(
                &mut data,
                AgentEvent::SubagentFinished {
                    run_id: 5,
                    task_index,
                    journal_run_id: format!("subagent-run-{task_index}"),
                    succeeded: false,
                    error: Some(error.into()),
                },
            );
        }

        assert_eq!(
            data.harness_activities[0].status,
            HarnessActivityStatus::Cancelled
        );
        assert_eq!(
            data.harness_activities[1].status,
            HarnessActivityStatus::Aborted
        );
    }

    #[test]
    fn stopped_generation_finalizes_streaming_and_running_tools() {
        let mut data = ChatData::default();
        data.push_tool("running".into(), "read_file".into(), "{}".into());
        data.push_stream_delta(StreamingKind::Thinking, "partial reasoning");

        data.mark_generation_stopped();

        assert!(data.streaming_text.is_empty());
        assert_eq!(data.streaming_kind, None);
        assert!(matches!(
            &data.messages[0],
            ChatMessage::Tool {
                status: ToolStatus::Cancelled,
                result_metadata,
                ..
            } if result_metadata.starts_with("Stopped · ")
        ));
        assert!(matches!(
            &data.messages[1],
            ChatMessage::Thinking { text } if text == "partial reasoning"
        ));
    }

    #[test]
    fn mark_generation_stopped_cancels_harness_activities() {
        let mut data = ChatData::default();
        data.harness_activities.push(HarnessActivity {
            key: "subagent-run-1".into(),
            task: "Inspect the repository".into(),
            agent: "scout".into(),
            status: HarnessActivityStatus::Working,
            detail: "Working".into(),
        });

        data.mark_generation_stopped();

        assert_eq!(
            data.harness_activities[0].status,
            HarnessActivityStatus::Cancelled
        );
    }

    #[test]
    fn tool_call_boundary_separates_preamble_from_final_assistant_text() {
        let mut data = ChatData::default();

        data.push_stream_delta(
            StreamingKind::Assistant,
            "Using startup guidance to check for applicable skills. ",
        );
        data.flush_tool_call_preamble();
        data.push_stream_delta(StreamingKind::Assistant, "Hey there! How can I help?");
        data.flush_streaming();

        assert!(matches!(
            &data.messages[0],
            ChatMessage::Thinking { text }
                if text == "Using startup guidance to check for applicable skills. "
        ));
        assert!(matches!(
            &data.messages[1],
            ChatMessage::Text {
                role: MsgRole::Assistant,
                text
            } if text == "Hey there! How can I help?"
        ));
    }

    #[test]
    fn identical_consecutive_thinking_is_not_duplicated() {
        let mut data = ChatData::default();

        data.push_thinking("Listing repository files and status".into());
        data.push_thinking("Listing repository files and status".into());

        assert_eq!(data.messages.len(), 1);
        assert!(matches!(
            &data.messages[0],
            ChatMessage::Thinking { text }
                if text == "Listing repository files and status"
        ));
    }

    #[test]
    fn distinct_consecutive_thinking_is_preserved() {
        let mut data = ChatData::default();

        data.push_thinking("Listing repository files".into());
        data.push_thinking("Reviewing manifests".into());

        assert!(matches!(
            &data.messages[0],
            ChatMessage::Thinking { text }
                if text == "Listing repository files\n\nReviewing manifests"
        ));
    }

    #[test]
    fn load_skill_has_a_compact_markdown_presentation() {
        let presentation = tool_presentation("load_skill", r#"{"name":"rust-review"}"#);

        assert_eq!(presentation.icon, ToolIcon::Skill);
        assert_eq!(presentation.title, "Load skill");
        assert_eq!(presentation.primary, "rust-review");
        assert_eq!(presentation.metadata, "skill instructions");
        assert_eq!(presentation.arguments_detail, "Skill ID: rust-review");
        assert!(presentation.output_markdown);
    }

    #[test]
    fn edit_file_presentation_summarizes_current_edit_payload() {
        let arguments = serde_json::json!({
            "path": "src/app.rs",
            "edits": [
                {"old_text": "old\nlines", "new_text": "new\nlines\nhere"},
                {"old_text": "", "new_text": "added"}
            ]
        })
        .to_string();
        let presentation = tool_presentation("edit_file", &arguments);

        assert_eq!(presentation.icon, ToolIcon::EditFile);
        assert_eq!(presentation.primary, "src/app.rs");
        assert_eq!(presentation.metadata, "+4 −2 lines");
    }

    #[test]
    fn search_tools_have_compact_presentations() {
        let grep = tool_presentation(
            "grep",
            r#"{"regex":"ActivityGroup","include_pattern":"src/**/*.rs"}"#,
        );
        let find = tool_presentation("find_path", r#"{"glob":"src/**/*.rs"}"#);

        assert_eq!(grep.title, "Search");
        assert_eq!(grep.primary, "ActivityGroup");
        assert_eq!(grep.metadata, "src/**/*.rs");
        assert_eq!(find.title, "Find files");
        assert_eq!(find.primary, "src/**/*.rs");
    }

    #[test]
    fn subagent_presentation_summarizes_parallel_tasks() {
        let arguments = serde_json::json!({
            "tasks": [
                {"agent": "scout", "task": "Inspect the repository structure"},
                {"agent": "reviewer", "task": "Review the security boundaries"}
            ],
            "parallel": true
        })
        .to_string();
        let presentation = tool_presentation("subagent", &arguments);

        assert_eq!(presentation.icon, ToolIcon::Subagent);
        assert_eq!(presentation.title, "Delegate");
        assert_eq!(presentation.primary, "2 tasks");
        assert_eq!(presentation.metadata, "parallel · scout, reviewer");
        assert!(presentation
            .arguments_detail
            .contains("Subagent Delegation (Parallel Execution)"));
        assert!(presentation
            .arguments_detail
            .contains("Subagent Session 1 (`scout`)"));
        assert!(presentation.output_markdown);
    }

    #[test]
    fn updating_a_tool_advances_the_chat_revision() {
        let mut data = ChatData::default();
        data.push_tool(
            "delegation".into(),
            "subagent".into(),
            "{\"tasks\":[]}".into(),
        );
        let revision = data.revision;

        data.update_tool("delegation", "[]".into(), Some(ToolStatus::Done));

        assert!(data.revision > revision);
    }

    #[test]
    fn subagent_rail_marks_only_the_first_sequential_task_as_running() {
        let arguments = serde_json::json!({
            "parallel": false,
            "tasks": [
                {"agent": "scout", "task": "Inspect the repository"},
                {"agent": "reviewer", "task": "Review the change"}
            ]
        })
        .to_string();

        let items = subagent_rail_items(&arguments, "", ToolStatus::Running, &[], None);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].agent, "scout");
        assert_eq!(items[0].status, "Working");
        assert_eq!(items[1].status, "Queued");
    }

    #[test]
    fn subagent_rail_places_tagged_child_tool_under_its_task() {
        let child_tool = |id: &str, name: &str, path: &str| ChatMessage::Tool {
            id: id.into(),
            name: name.into(),
            arguments: format!(r#"{{"path":"{path}"}}"#),
            output: String::new(),
            status: ToolStatus::Running,
            presentation: tool_presentation(name, &format!(r#"{{"path":"{path}"}}"#)),
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        };
        let messages = [
            child_tool("subagent-40:0:stale", "read_file", "src/stale.rs"),
            child_tool("subagent-41:0:read", "read_file", "src/lib.rs"),
            child_tool("subagent-41:1:read", "read_file", "src/state.rs"),
            child_tool(
                "subagent-not-a-run:0:ignored",
                "write_file",
                "src/ignored.rs",
            ),
        ];

        let items = subagent_rail_items(
            r#"{"parallel":true,"tasks":[{"agent":"scout","task":"inspect"},{"agent":"reviewer","task":"review"}]}"#,
            "",
            ToolStatus::Running,
            &messages,
            Some(41),
        );

        assert!(items[0].detail.contains("read_file"));
        assert!(items[0].detail.contains("src/lib.rs"));
        assert!(!items[0].detail.contains("src/stale.rs"));
        assert!(!items[0].detail.contains("src/state.rs"));
        assert!(!items[0].detail.contains("src/ignored.rs"));
        assert!(items[1].detail.contains("read_file"));
        assert!(items[1].detail.contains("src/state.rs"));
        assert!(!items[1].detail.contains("src/lib.rs"));
        assert!(!items[1].detail.contains("src/ignored.rs"));
    }

    #[test]
    fn running_subagent_rows_use_their_own_child_run() {
        let parent = |id: &str| ChatMessage::Tool {
            id: id.into(),
            name: "subagent".into(),
            arguments: r#"{"tasks":[{"agent":"worker","task":"inspect"}]}"#.into(),
            output: String::new(),
            status: ToolStatus::Running,
            presentation: tool_presentation("subagent", "{}"),
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        };
        let child = |id: &str, path: &str| ChatMessage::Tool {
            id: id.into(),
            name: "read_file".into(),
            arguments: format!(r#"{{"path":"{path}"}}"#),
            output: String::new(),
            status: ToolStatus::Running,
            presentation: tool_presentation("read_file", &format!(r#"{{"path":"{path}"}}"#)),
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        };
        let messages = [
            parent("first"),
            child("subagent-40:0:first", "src/first.rs"),
            parent("second"),
            child("subagent-41:0:second", "src/second.rs"),
        ];

        let first = subagent_rail_items(
            r#"{"tasks":[{"agent":"worker","task":"inspect"}]}"#,
            "",
            ToolStatus::Running,
            &messages[..2],
            Some(40),
        );
        let second = subagent_rail_items(
            r#"{"tasks":[{"agent":"worker","task":"inspect"}]}"#,
            "",
            ToolStatus::Running,
            &messages,
            Some(41),
        );

        assert!(first[0].detail.contains("src/first.rs"));
        assert!(second[0].detail.contains("src/second.rs"));
    }

    #[test]
    fn completed_subagent_rail_items_keep_their_persisted_detail() {
        let output = serde_json::json!([{
            "agent": "scout",
            "task": "Inspect the workspace",
            "status": "Done",
            "thinking": "I will inspect the entry point.",
            "inner_tools": [{
                "name": "read_file",
                "target_preview": "src/main.rs",
                "is_error": false
            }],
            "output": "The entry point is ready."
        }])
        .to_string();

        let items = subagent_rail_items("", &output, ToolStatus::Done, &[], None);

        assert_eq!(items.len(), 1);
        assert!(items[0].detail.contains("I will inspect the entry point."));
        assert!(items[0].detail.contains("read_file"));
        assert!(items[0].detail.contains("src/main.rs"));
        assert!(items[0].detail.contains("The entry point is ready."));
    }

    #[test]
    fn collapsed_thinking_preview_removes_common_markdown_syntax() {
        let preview = collapsed_thinking_preview(
            "# **Plan**\n- Inspect `state.rs`\n2. Keep _snake_case_ and ~~finish~~",
            200,
        );

        assert_eq!(preview, "Plan Inspect state.rs Keep snake_case and finish");
    }

    #[test]
    fn collapsed_thinking_preview_normalizes_then_truncates() {
        assert_eq!(
            collapsed_thinking_preview("  **One**\n\n  `two`   three  ", 11),
            "One two th…"
        );
        assert_eq!(collapsed_thinking_preview("anything", 0), "");
    }

    #[test]
    fn collapsed_thinking_preview_preserves_literal_shell_and_math_punctuation() {
        assert_eq!(
            collapsed_thinking_preview("Inspect ~/bin and calculate a*b", 80),
            "Inspect ~/bin and calculate a*b"
        );
    }

    #[test]
    fn tool_result_preview_uses_readable_text_from_multiple_lines() {
        assert_eq!(
            tool_result_preview("## Result\n- **Three** files changed", 80),
            "Result Three files changed"
        );
        assert_eq!(tool_result_preview(" \n\t", 80), "(empty output)");
    }

    #[test]
    fn tool_result_detail_caps_lines_and_reports_omissions() {
        let output = (1..=25)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let detail = tool_result_detail(&output, 1_000);

        assert_eq!(detail.lines().count(), 20);
        assert!(detail.starts_with("line 1\n"));
        assert!(detail.contains("line 19\n"));
        assert!(detail.ends_with("… 6 lines omitted"));
    }

    #[test]
    fn tool_result_detail_honors_character_bound_and_preserves_notice() {
        let detail = tool_result_detail(&"x".repeat(100), 40);

        assert!(detail.chars().count() <= 40);
        assert!(detail.ends_with("… output truncated"));
        assert_eq!(detail.lines().count(), 2);
    }

    #[test]
    fn tool_result_detail_preserves_empty_output_behavior() {
        assert_eq!(tool_result_detail(" \n\t", 100), "(empty output)");
    }

    #[test]
    fn specialized_tool_completion_metadata_is_action_oriented() {
        assert_eq!(
            result_metadata_for_tool(
                "load_skill",
                "instructions",
                ToolStatus::Done,
                Duration::from_secs(2)
            ),
            "Loaded · 2.0s"
        );
        assert_eq!(
            result_metadata_for_tool(
                "subagent",
                "report",
                ToolStatus::Done,
                Duration::from_secs(3)
            ),
            "Completed · 3.0s"
        );
        assert_eq!(
            result_metadata_for("one line", ToolStatus::Done, Duration::from_secs(1)),
            "1 line · 8 B · 1.0s"
        );
    }
}
