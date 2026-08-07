//! App shell: script_mod! DSL, startup/auth wiring, agent event pump.
//!
//! Chat, sessions, and command palette panels are modularized under `crate::panels`.

mod settings_handlers;
mod terminal_handlers;
mod workspace_sync;

use crate::components::code_editor_view::{CodeEditorViewAction, CodeEditorViewWidgetRefExt};
use crate::components::context_window::ContextWindowWidgetRefExt;
use crate::components::file_tree::{FileTree, FileTreeAction};
use crate::components::git_changes::{GitChanges, GitChangesAction};
use crate::components::git_diff::GitDiffView;
use crate::components::model_dropdown::IconDropDownWidgetRefExt;
use crate::components::project_header::ProjectHeaderAction;
use crate::components::provider_settings_modal::{
    ProviderSettingsModal, ProviderSettingsModalAction, SettingsPage,
};
use crate::components::session_row::SessionRowAction;
use crate::components::task_sidebar::{
    task_header_state, TaskSidebar, TaskSidebarAction, TaskSidebarItem,
};
use crate::components::terminal_panel::ProjectTerminalWidgetRefExt;
use crate::git::GitStatus;
use crate::panels::chat::state::{reduce_harness_event, HarnessActivity};
use crate::panels::chat::{
    accepts_generation_event, concise_status, draft_for_cancellation, submitted_draft, ChatList,
    ChatListWidgetRefExt, ComposerState, ComposerStatus, GenerationEvent, StarterPromptAction,
    SubagentRail, SubagentRailAction, ToolFoldHeader,
};
use crate::panels::command_palette::*;
use terminal_handlers::canonical_terminal_work_dir;
#[cfg(test)]
use terminal_handlers::truncate_terminal_output;

use crate::panels::sessions::{
    set_search_query, ProjectRegistry, SessionContextMenu, SessionContextMenuAction, SessionList,
    SessionListAction,
};
use crate::state::{
    active_session_entry, archive_session, begin_title_generation, builtin_commands,
    create_new_session, delete_session, end_title_generation, is_project_working,
    is_session_working, normalize_session_title, project_work_dir_at_row, refresh_sessions,
    session_entry_at_row, session_entry_for_file, session_health, session_overflow_at_row,
    session_title_eligible, set_active_project, set_active_session, set_session_context_target,
    set_session_health, set_session_working, title_prompt_for_submission, toggle_project_collapsed,
    toggle_project_show_all, truncate_chars, CapabilityState, CommandInfo, GuiAgentEvent, MsgRole,
    SessionEntry, ToolStatus,
};
use crate::updater::UpdateStatus;
use crate::workspace::{AppState, SessionKey, WorkspaceUiState};
use base64::Engine as _;
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;
use robius_file_picker::FileDialog;
use threadlane_agent::harness::{
    EventPayload, JsonlStore, OperationOutcome, Record as HarnessRecord, Reducer, StreamingState,
};
use threadlane_agent::{
    get_runtime, AgentEvent, ImageAttachment, ReasoningEffort, SessionPlan, TokenUsage,
};
use threadlane_coding_agent::{
    cancel_open_subagent_operations, default_global_threadlane_dir, discover_agents, AgentConfig,
    AgentScope, CapabilityCatalog, CodingAgent, CodingAgentCancellation, CodingAgentOptions,
    CodingAgentWorkHandle, ExtensionManager, ExtensionScope, HarnessSupervisor, ProjectContext,
    SkillMetadata, SkillSettings, TaskKind, TaskRecord,
};
use threadlane_provider::auth;
use threadlane_provider::openai::fetch_available_models;
use threadlane_provider::ProviderClient;

use crate::panels::terminal::{ProjectTerminalGroup, ProjectTerminalSession};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

const ANTIGRAVITY_MODELS: &[&str] = &[
    "antigravity/gemini-3.6-flash",
    "antigravity/gemini-3.5-flash",
    "antigravity/gemini-3.1-pro",
    "antigravity/claude-sonnet-4-6",
    "antigravity/claude-opus-4-6",
    "antigravity/gpt-oss-120b",
];
const OPENCODE_GO_MODELS: &[&str] = &[
    "opencode-go/mimo-v2.5-pro",
    "opencode-go/mimo-v2.5",
    "opencode-go/qwen3.8-max",
    "opencode-go/minimax-m3",
    "opencode-go/minimax-m2.7",
    "opencode-go/deepseek-v4-pro",
    "opencode-go/deepseek-v4-flash",
    "opencode-go/hy3",
];
const MAX_TERMINAL_OUTPUT: usize = 256 * 1024;
const RIGHT_SIDEBAR_MIN_WIDTH: f64 = 220.0;
const RIGHT_SIDEBAR_MAX_WIDTH: f64 = 520.0;
const RIGHT_SIDEBAR_MIN_MAIN_WIDTH: f64 = 360.0;
const LEFT_SIDEBAR_WIDTH: f64 = 250.0;
const DEFAULT_CONTEXT_WINDOW: u32 = 258_000;
const CONTEXT_USAGE_FACT: &str = "context_window_usage";

fn left_sidebar_splitter_align(open: bool) -> SplitterAlign {
    SplitterAlign::FromA(if open { LEFT_SIDEBAR_WIDTH } else { 0.0 })
}

fn normalize_generated_commit_message(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```"))
        .unwrap_or_default()
        .trim_matches('`')
        .trim();
    let line = line
        .strip_prefix("Commit message:")
        .or_else(|| line.strip_prefix("Commit:"))
        .unwrap_or(line)
        .trim();
    truncate_chars(line, 72)
}

fn context_window_limit(_model: &str) -> u32 {
    DEFAULT_CONTEXT_WINDOW
}

fn restore_harness_activities(session_file: &Path) -> Vec<HarnessActivity> {
    let mut activities = Vec::new();
    match JsonlStore::open_read_only(session_file) {
        Ok(store) => {
            if let Ok(state) = Reducer::reduce(&store) {
                let v2_subagent_runs = store.records().iter().filter_map(|record| match record {
                    HarnessRecord::OperationStarted { id, lane, seq, .. } if lane != "main" => {
                        Some((id.clone(), lane.clone(), *seq))
                    }
                    _ => None,
                });
                for (run_id, lane_name, _started_seq) in v2_subagent_runs {
                    let Some(task) = store
                        .entries()
                        .iter()
                        .filter_map(|entry| {
                            (entry.lane == lane_name).then(|| match &entry.message {
                                threadlane_agent::AgentMessage::User { content }
                                | threadlane_agent::AgentMessage::UserWithImages {
                                    content, ..
                                } => content.clone(),
                                _ => String::new(),
                            })
                        })
                        .find(|task| !task.trim().is_empty())
                    else {
                        continue;
                    };
                    let finished = store
                        .records()
                        .iter()
                        .rev()
                        .find_map(|record| match record {
                            HarnessRecord::OperationFinished {
                                run_id: record_run_id,
                                outcome,
                                error,
                                ..
                            } if record_run_id == &run_id => Some((outcome, error.clone())),
                            _ => None,
                        });
                    let (status, detail) = match finished {
                        Some((OperationOutcome::Completed, error)) => (
                            crate::panels::chat::state::HarnessActivityStatus::Recovered,
                            error.unwrap_or_else(|| "Completed".into()),
                        ),
                        Some((OperationOutcome::Aborted, error)) => (
                            crate::panels::chat::state::HarnessActivityStatus::Cancelled,
                            error.unwrap_or_else(|| "Cancelled".into()),
                        ),
                        Some((OperationOutcome::Failed | OperationOutcome::Declined, error)) => (
                            crate::panels::chat::state::HarnessActivityStatus::Aborted,
                            error.unwrap_or_else(|| "Aborted".into()),
                        ),
                        None => (
                            crate::panels::chat::state::HarnessActivityStatus::Recovering,
                            store
                                .records()
                                .iter()
                                .find_map(|record| match record {
                                    HarnessRecord::OperationStarted { id, lane, .. }
                                        if id == &run_id =>
                                    {
                                        state
                                            .lane(lane)
                                            .map(|lane| harness_lane_activity(lane, None))
                                    }
                                    _ => None,
                                })
                                .unwrap_or_else(|| {
                                    "Suspended operation; resume or abort before continuing".into()
                                }),
                        ),
                    };
                    activities.push(HarnessActivity {
                        key: run_id,
                        task,
                        agent: "subagent".into(),
                        status,
                        detail,
                    });
                }
                if let Some(lane) = state.lane("main") {
                    if let Some(run_id) = lane.open_operation.as_deref() {
                        let start = store.records().iter().find_map(|record| match record {
                            HarnessRecord::OperationStarted {
                                id,
                                seq,
                                source_leaf_id,
                                ..
                            } if id == run_id => Some((*seq, source_leaf_id.as_deref())),
                            _ => None,
                        });
                        let task = start
                            .and_then(|(start_seq, source_leaf_id)| {
                                let source_seq = source_leaf_id.and_then(|id| {
                                    store
                                        .entries()
                                        .iter()
                                        .find(|entry| entry.id == id)
                                        .map(|entry| entry.seq)
                                });
                                store.entries().iter().rev().find_map(|entry| {
                                    (entry.seq <= start_seq
                                        && source_seq.map_or(true, |source| entry.seq > source))
                                    .then(|| match &entry.message {
                                        threadlane_agent::AgentMessage::User { content }
                                        | threadlane_agent::AgentMessage::UserWithImages {
                                            content,
                                            ..
                                        } => content.clone(),
                                        _ => String::new(),
                                    })
                                })
                            })
                            .filter(|task| !task.trim().is_empty())
                            .unwrap_or_else(|| "Foreground operation".into());
                        activities.push(HarnessActivity {
                            key: format!("main-{run_id}"),
                            task,
                            agent: "main".into(),
                            status: if lane.abort_requested {
                                crate::panels::chat::state::HarnessActivityStatus::Aborted
                            } else {
                                crate::panels::chat::state::HarnessActivityStatus::Recovering
                            },
                            detail: harness_lane_activity(lane, None),
                        });
                    }
                }
            }
        }
        Err(error) if session_file.exists() => activities.push(HarnessActivity {
            key: "main-harness-fault".into(),
            task: "Harness storage".into(),
            agent: "main".into(),
            status: crate::panels::chat::state::HarnessActivityStatus::Faulted,
            detail: format!("Harness storage fault: {error}"),
        }),
        Err(_) => {}
    }
    activities
}

fn harness_activities_from_snapshot(
    snapshot: &threadlane_agent::harness::Snapshot,
) -> Vec<HarnessActivity> {
    use crate::panels::chat::state::HarnessActivityStatus;
    let mut activities = Vec::new();
    for (run_id, lane_name) in snapshot.records.iter().filter_map(|record| match record {
        HarnessRecord::OperationStarted { id, lane, .. } if lane != "main" => {
            Some((id.as_str(), lane.as_str()))
        }
        _ => None,
    }) {
        let Some(task) = snapshot
            .entries
            .iter()
            .find_map(|entry| {
                (entry.lane == lane_name).then(|| match &entry.message {
                    threadlane_agent::AgentMessage::User { content }
                    | threadlane_agent::AgentMessage::UserWithImages { content, .. } => {
                        content.clone()
                    }
                    _ => String::new(),
                })
            })
            .filter(|task| !task.trim().is_empty())
        else {
            continue;
        };
        let finished = snapshot
            .records
            .iter()
            .rev()
            .find_map(|record| match record {
                HarnessRecord::OperationFinished {
                    run_id: record_run_id,
                    outcome,
                    error,
                    ..
                } if record_run_id == run_id => Some((outcome, error.clone())),
                _ => None,
            });
        let (status, detail) = match finished {
            Some((OperationOutcome::Completed, error)) => (
                HarnessActivityStatus::Recovered,
                error.unwrap_or_else(|| "Completed".into()),
            ),
            Some((OperationOutcome::Aborted, error)) => (
                HarnessActivityStatus::Cancelled,
                error.unwrap_or_else(|| "Cancelled".into()),
            ),
            Some((OperationOutcome::Failed | OperationOutcome::Declined, error)) => (
                HarnessActivityStatus::Aborted,
                error.unwrap_or_else(|| "Aborted".into()),
            ),
            None => (
                HarnessActivityStatus::Recovering,
                snapshot
                    .state
                    .lane(lane_name)
                    .map(|lane| harness_lane_activity(lane, snapshot.streaming.as_ref()))
                    .unwrap_or_else(|| {
                        "Suspended operation; resume or abort before continuing".into()
                    }),
            ),
        };
        activities.push(HarnessActivity {
            key: run_id.into(),
            task,
            agent: "subagent".into(),
            status,
            detail,
        });
    }
    if let Some(lane) = snapshot.state.lane("main") {
        if let Some(run_id) = lane.open_operation.as_deref() {
            let task = snapshot
                .records
                .iter()
                .find_map(|record| match record {
                    HarnessRecord::OperationStarted {
                        id,
                        seq,
                        source_leaf_id,
                        ..
                    } if id == run_id => Some((*seq, source_leaf_id.as_deref())),
                    _ => None,
                })
                .and_then(|(start_seq, source_leaf_id)| {
                    let source_seq = source_leaf_id.and_then(|id| {
                        snapshot
                            .entries
                            .iter()
                            .find(|entry| entry.id == id)
                            .map(|entry| entry.seq)
                    });
                    snapshot.entries.iter().rev().find_map(|entry| {
                        (entry.seq <= start_seq
                            && source_seq.map_or(true, |source| entry.seq > source))
                        .then(|| match &entry.message {
                            threadlane_agent::AgentMessage::User { content }
                            | threadlane_agent::AgentMessage::UserWithImages { content, .. } => {
                                content.clone()
                            }
                            _ => String::new(),
                        })
                    })
                })
                .filter(|task| !task.trim().is_empty())
                .unwrap_or_else(|| "Foreground operation".into());
            activities.push(HarnessActivity {
                key: format!("main-{run_id}"),
                task,
                agent: "main".into(),
                status: if lane.abort_requested {
                    HarnessActivityStatus::Aborted
                } else if snapshot
                    .streaming
                    .as_ref()
                    .is_some_and(|stream| stream.lane == lane.name)
                {
                    HarnessActivityStatus::Working
                } else {
                    HarnessActivityStatus::Recovering
                },
                detail: harness_lane_activity(lane, snapshot.streaming.as_ref()),
            });
        }
    }
    activities
}

fn harness_lane_activity(
    lane: &threadlane_agent::harness::LaneState,
    streaming: Option<&threadlane_agent::harness::StreamingState>,
) -> String {
    let action = lane
        .tools
        .iter()
        .rev()
        .find(|tool| !tool.completed)
        .map(|tool| {
            format!(
                "Running tool: {} · {}",
                tool.tool_name,
                match tool.replay {
                    threadlane_agent::harness::ToolReplaySafety::Safe => "replay-safe",
                    threadlane_agent::harness::ToolReplaySafety::Never => "no replay",
                }
            )
        })
        .or_else(|| {
            streaming.and_then(|stream| {
                if stream.lane == lane.name && !stream.tool_call_ids.is_empty() {
                    Some(format!("Calling {} tool(s)", stream.tool_call_ids.len()))
                } else if stream.lane == lane.name && !stream.reasoning.is_empty() {
                    Some("Thinking".into())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| match lane.status {
            threadlane_agent::harness::LaneStatus::SuspendedCrash
            | threadlane_agent::harness::LaneStatus::SuspendedDeferred => {
                "Recovering suspended operation".into()
            }
            _ => "Working".into(),
        });

    let mut detail = action;
    let mut queue_counts = Vec::new();
    for (queue, label) in [
        (threadlane_agent::harness::QueueKind::Steer, "steer"),
        (threadlane_agent::harness::QueueKind::FollowUp, "follow-up"),
        (threadlane_agent::harness::QueueKind::NextRun, "next-run"),
    ] {
        let count = lane
            .queued
            .iter()
            .filter(|entry| entry.queue == queue)
            .count();
        if count > 0 {
            queue_counts.push(format!("{label} {count}"));
        }
    }
    if !queue_counts.is_empty() {
        detail.push_str(" · queued: ");
        detail.push_str(&queue_counts.join(", "));
    }
    if lane.usage.total_tokens > 0 {
        detail.push_str(" · ");
        detail.push_str(&format_token_count(lane.usage.total_tokens));
        detail.push_str(" tokens");
    }
    if !lane.deferred_writes.is_empty() {
        detail.push_str(&format!(
            " · {} deferred write(s)",
            lane.deferred_writes.len()
        ));
    }
    detail
}

fn format_token_count(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f32 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f32 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn suppress_live_main_recovery(activities: &mut Vec<HarnessActivity>, live: bool) {
    if live {
        activities.retain(|activity| {
            !(activity.agent == "main"
                && activity.status == crate::panels::chat::state::HarnessActivityStatus::Recovering)
        });
    }
}

fn harness_live_streaming_detail(stream: &StreamingState) -> String {
    if !stream.tool_call_ids.is_empty() {
        let count = stream.tool_call_ids.len();
        if count == 1 {
            "Using tool".into()
        } else {
            format!("Using {count} tools")
        }
    } else if !stream.assistant_text.is_empty() {
        "Responding".into()
    } else if !stream.reasoning.is_empty() {
        "Thinking".into()
    } else {
        "Working".into()
    }
}

fn live_main_activity(detail: impl Into<String>) -> HarnessActivity {
    HarnessActivity {
        key: "main-live".into(),
        task: "Foreground agent".into(),
        agent: "main".into(),
        status: crate::panels::chat::state::HarnessActivityStatus::Working,
        detail: detail.into(),
    }
}

fn set_live_main_activity(
    chat: &mut crate::panels::chat::state::ChatData,
    detail: impl Into<String>,
) {
    crate::panels::chat::state::reduce_harness_activity(
        &mut chat.harness_activities,
        live_main_activity(detail),
    );
    chat.revision = chat.revision.wrapping_add(1);
}

fn clear_live_main_activity(chat: &mut crate::panels::chat::state::ChatData) {
    let before = chat.harness_activities.len();
    chat.harness_activities
        .retain(|activity| activity.key != "main-live");
    if chat.harness_activities.len() != before {
        chat.revision = chat.revision.wrapping_add(1);
    }
}

fn background_task_harness_activity(
    task_id: &str,
    task: &TaskRecord,
    event: &AgentEvent,
) -> Option<HarnessActivity> {
    use crate::panels::chat::state::HarnessActivityStatus;
    let (status, detail) = match event {
        AgentEvent::AgentStart => (
            HarnessActivityStatus::Working,
            task.current_activity
                .clone()
                .unwrap_or_else(|| "Working on task".into()),
        ),
        AgentEvent::AgentEnd { usage } => {
            let mut detail = "Task completed".to_string();
            if usage.total_tokens > 0 {
                detail.push_str(&format!(" · {:.1}k tokens", usage.total_tokens as f32 / 1000.0));
            }
            (HarnessActivityStatus::Recovered, detail)
        }
        AgentEvent::AgentError { error } => (
            HarnessActivityStatus::Aborted,
            error.clone(),
        ),
        AgentEvent::TurnStart { .. } | AgentEvent::MessageStart { .. } => (
            HarnessActivityStatus::Working,
            "Generating response".into(),
        ),
        AgentEvent::ToolExecutionStart { name, .. } => {
            (HarnessActivityStatus::Working, format!("Using tool: {name}"))
        }
        AgentEvent::SubagentQueued { .. } => (
            HarnessActivityStatus::Working,
            "Delegating subtask".into(),
        ),
        AgentEvent::SubagentStarted { .. } => (
            HarnessActivityStatus::Working,
            "Subtasks running".into(),
        ),
        AgentEvent::SubagentFinished { succeeded, error, .. } => {
            if *succeeded {
                (
                    HarnessActivityStatus::Working,
                    "Subtasks completed".into(),
                )
            } else {
                (
                    HarnessActivityStatus::Working,
                    error
                        .as_deref()
                        .unwrap_or("Subtask issue")
                        .into(),
                )
            }
        }
        AgentEvent::SubagentRecovery { detail, .. } => {
            let detail_text = detail
                .clone()
                .unwrap_or_else(|| "Recovery in progress".into());
            (HarnessActivityStatus::Working, detail_text)
        }
        _ => return None,
    };
    Some(HarnessActivity {
        key: format!("bg-task-{task_id}"),
        task: if task.summary.is_empty() {
            "Background task".into()
        } else {
            task.summary.clone()
        },
        agent: "Background task".into(),
        status,
        detail,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RightSidebarTab {
    #[default]
    Git,
    Tasks,
    FileTree,
    Editor,
}

fn append_antigravity_models(models: &mut Vec<String>) {
    for model in ANTIGRAVITY_MODELS {
        if !models.iter().any(|existing| existing == model) {
            models.push((*model).to_string());
        }
    }
}

fn append_opencode_models(models: &mut Vec<String>) {
    if threadlane_provider::opencode_auth::load_opencode_api_key().is_none() {
        return;
    }
    for model in OPENCODE_GO_MODELS {
        if !models.iter().any(|existing| existing == model) {
            models.push((*model).to_string());
        }
    }
}

fn include_connected_provider_models(mut models: Vec<String>) -> Vec<String> {
    if threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some() {
        append_antigravity_models(&mut models);
    }
    append_opencode_models(&mut models);
    models
}

/// Adds a pseudo-model per enabled ACP agent.
///
/// Reuses the `provider/model` id convention so external agents appear in the
/// existing picker, persist per session, and work with `/model`, instead of
/// needing a second selection mechanism.
fn append_acp_models(models: &mut Vec<String>, global_dir: Option<&Path>, work_dir: Option<&Path>) {
    let manager = threadlane_coding_agent::AcpManager::new(
        global_dir.map(Path::to_path_buf),
        work_dir.map(Path::to_path_buf),
    );
    for config in manager.configs() {
        if !config.enabled {
            continue;
        }
        let model = threadlane_coding_agent::acp_model_id(&config.id);
        if !models.iter().any(|existing| *existing == model) {
            models.push(model);
        }
    }
}

/// Whether the user can talk to at least one LLM right now, via any
/// supported provider (not just OpenAI/ChatGPT specifically).
fn has_connected_provider() -> bool {
    auth::load_credentials().is_some()
        || threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some()
        || threadlane_provider::opencode_auth::load_opencode_api_key().is_some()
}

fn ordered_model_options(
    models: Vec<String>,
    selected_model: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let mut canonical = Vec::new();
    for model in models {
        if !model.is_empty() && !canonical.contains(&model) {
            canonical.push(model);
        }
    }
    if !selected_model.is_empty() && !canonical.iter().any(|model| model == selected_model) {
        canonical.push(selected_model.to_string());
    }
    canonical.sort_by_key(|model| {
        (
            threadlane_provider::router::is_antigravity_model(model),
            threadlane_provider::router::is_opencode_model(model),
        )
    });
    if canonical.is_empty() {
        return None;
    }

    let selected_model = if selected_model.is_empty() {
        canonical[0].clone()
    } else {
        selected_model.to_string()
    };
    let mut display = canonical.clone();
    display.retain(|model| model != &selected_model);
    display.push(selected_model);
    Some((canonical, display))
}

fn default_model_name() -> &'static str {
    let has_openai = auth::load_credentials().is_some()
        || std::env::var("OPENAI_API_KEY")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false);
    let has_antigravity =
        threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some();
    let has_opencode = threadlane_provider::opencode_auth::load_opencode_api_key().is_some();
    if !has_openai && !has_opencode && has_antigravity {
        "antigravity/gemini-3.6-flash"
    } else if !has_openai && !has_antigravity && has_opencode {
        OPENCODE_GO_MODELS[0]
    } else {
        "gpt-5.6-luna"
    }
}

fn model_credential_error(
    model: &str,
    has_openai_credentials: bool,
    has_antigravity_credentials: bool,
    has_opencode_credentials: bool,
) -> Option<&'static str> {
    if threadlane_provider::router::is_antigravity_model(model) {
        (!has_antigravity_credentials)
            .then_some("Sign in with Google Antigravity before using this model.")
    } else if threadlane_provider::router::is_opencode_model(model) {
        (!has_opencode_credentials)
            .then_some("Set an OpenCode API key in Settings or OPENCODE_API_KEY.")
    } else {
        (!has_openai_credentials)
            .then_some("Please provide an OpenAI API key or click 'Login ChatGPT' to authenticate.")
    }
}

fn user_home_dir() -> Option<PathBuf> {
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
}

fn global_threadlane_dir() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".threadlane")
}

fn resolve_initial_launch_dir() -> PathBuf {
    let home = user_home_dir();
    let current = std::env::current_dir()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok());

    if let Some(dir) = current {
        let is_root = dir.parent().is_none();
        let is_app_bundle = dir
            .components()
            .any(|c| c.as_os_str().to_string_lossy().ends_with(".app"));
        let is_writable = std::fs::metadata(&dir)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false);

        if !is_root && !is_app_bundle && is_writable {
            return dir;
        }
    }

    home.unwrap_or_else(|| PathBuf::from("."))
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

use crate::path_utils::compact_workspace_path;

script_mod! {
    use mod.prelude.widgets.*
    use mod.components.*

    // -------------------------------------------------------------------
    // Chat message list: bubbles + markdown + streaming tail
    // -------------------------------------------------------------------
    let ChatList = #(ChatList::register_widget(vm)) {
        width: Fill
        height: Fill
        flow: Overlay

        // ── Empty-session welcome overlay (shown when no messages) ──────────
        empty_state := View {
            width: Fill
            height: Fill
            flow: Down
            align: Align{x: 0.5 y: 0.5}
            spacing: 0
            padding: Inset{bottom: 60}
            visible: false

            // Hero icon — Threadlane logo in a rounded tile
            empty_icon_wrap := RoundedView {
                width: 60
                height: 60
                margin: Inset{bottom: 22}
                align: Align{x: 0.5 y: 0.5}
                draw_bg +: {
                    color: theme.color_background
                    border_color: theme.color_card
                    border_size: 1.0
                    border_radius: 14.0
                }
                empty_hero_icon := Icon {
                    width: 28
                    height: 28
                    icon_walk: Walk{width: 28 height: 28}
                    draw_icon +: {
                        svg: crate_resource("self:resources/icons/logo.svg")
                        color: theme.color_primary
                    }
                }
            }

            // Headline: "What should we build in {project}?"
            headline_row := View {
                width: Fit
                height: Fit
                flow: Right
                align: Align{y: 0.5}
                margin: Inset{bottom: 6}

                headline_pre_lbl := mod.components.HeadlineLabel {
                    text: "What should we build in "
                }
                project_name_inline_lbl := mod.components.HeadlineAccentLabel {
                    text: ""
                }
                headline_post_lbl := mod.components.HeadlineLabel {
                    text: "?"
                }
            }

            // Workspace path subtitle badge
            workspace_path_wrap := mod.components.WorkspaceBadge {}

            // Action suggestion cards
                cards_row := View {
                    width: Fit
                    height: Fit
                    flow: Right
                    spacing: 10

                    explore_card := mod.components.StarterPromptCard {
                        content.header.icon_wrap.draw_bg.color: theme.color_primary_tint
                        content.header.icon_wrap.draw_bg.border_color: theme.color_primary_tint
                        content.header.icon_wrap.icon.draw_icon.svg: crate_resource("self:resources/icons/read-file.svg")
                        content.header.title.text: "Explore code"
                        content.description.text: "Understand the structure and key files"
                    }
                    build_card := mod.components.StarterPromptCard {
                        content.header.icon_wrap.draw_bg.color: theme.color_accent_tint
                        content.header.icon_wrap.draw_bg.border_color: theme.color_accent_tint
                        content.header.icon_wrap.icon.draw_icon.svg: crate_resource("self:resources/icons/write-file.svg")
                        content.header.title.text: "Build something"
                        content.description.text: "Start a feature, app, or tool"
                    }
                    review_card := mod.components.StarterPromptCard {
                        content.header.icon_wrap.draw_bg.color: theme.color_success_tint
                        content.header.icon_wrap.draw_bg.border_color: theme.color_success_tint
                        content.header.icon_wrap.icon.draw_icon.svg: crate_resource("self:resources/icons/edit-file.svg")
                        content.header.icon_wrap.icon.draw_icon.color: theme.color_success
                        content.header.title.text: "Review code"
                        content.description.text: "Find bugs and simplify changes"
                    }
                    fix_card := mod.components.StarterPromptCard {
                        content.header.icon_wrap.draw_bg.color: theme.color_destructive_tint
                        content.header.icon_wrap.draw_bg.border_color: theme.color_destructive_tint
                        content.header.icon_wrap.icon.draw_icon.svg: crate_resource("self:resources/icons/tool.svg")
                        content.header.icon_wrap.icon.draw_icon.color: theme.color_warning
                        content.header.title.text: "Fix an issue"
                        content.description.text: "Diagnose errors and failures"
                    }
                }
        }

        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            // Tail only while already at the bottom. PortalList's auto_tail resets the
            // scroll position during every apply, which can swallow the first wheel events.
            auto_tail: false
            smooth_tail: false
            selectable: true
            reuse_items: false

            UserMsg := mod.components.UserMsgBase {}

            UserMsgWrapped := mod.components.UserMsgBase {
                user_bubble +: {
                    width: Fill{max: 680}
                    md +: { width: Fill }
                }
            }

            AssistantMsg := View {
                width: Fill
                height: Fit
                align: Align{x: 0.0}
                margin: Inset{top: 8 bottom: 12 left: 34 right: 66}

                md := mod.components.ChatMarkdown {
                    width: Fill{max: 934}
                }
            }

            SystemMsg := View {
                width: Fill
                height: Fit
                margin: Inset{top: 3 bottom: 3 left: 20 right: 24}
                lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: theme.color_muted_foreground
                        text_style +: { font_size: 10.0 }
                    }
                }
            }

            ActivityGroupMsg := #(ToolFoldHeader::register_widget(vm)) {
                width: Fill
                height: Fit
                flow: Down
                body_walk: Walk{width: Fill, height: Fit}
                margin: Inset{top: 4 bottom: 2 left: 20 right: 24}
                opened: 0.0
                animator +: { active: { default: @off } }
                header := mod.components.ActivityHeader {
                    height: 28
                    title_lbl := Label {
                        width: 62
                        height: Fit
                        text: "Worked"
                        draw_text +: {
                            color: theme.color_muted_foreground
                            text_style: theme.font_bold { font_size: 9.0 }
                        }
                    }
                    summary := View {
                        width: Fill
                        height: 20
                        flow: Right
                        spacing: 7
                        align: Align{y: 0.5}
                        clip_x: true
                        preview_lbl := mod.components.ClippedLabel {
                            draw_text +: { color: theme.color_muted_foreground }
                        }
                        status_indicator := ActivityStatusIndicator {}
                    }
                }
                body := RoundedView {
                    width: Fill
                    height: Fit
                    padding: Inset{left: 30 top: 3 right: 18 bottom: 7}
                    draw_bg +: {
                        color: theme.color_transparent
                        border_size: 0.0
                    }
                    md := mod.components.ChatMarkdown {}
                }
            }

            ThinkingMsg := #(ToolFoldHeader::register_widget(vm)) {
                width: Fill
                height: Fit
                flow: Down
                body_walk: Walk{width: Fill, height: Fit}
                margin: Inset{top: 4 bottom: 2 left: 20 right: 24}
                opened: 0.0
                animator +: { active: { default: @off } }
                header := mod.components.ActivityHeader {
                    icon_tile := View {
                        width: 20
                        height: 20
                        align: Align{x: 0.5 y: 0.5}
                        icon_stack := View {
                            width: 14
                            height: 14
                            flow: Overlay
                            icon_generic := mod.components.ActivitySvgIcon { visible: false }
                            icon_thinking := mod.components.ActivitySvgIcon {
                                visible: true
                                icon +: { draw_icon +: { color: theme.color_muted_foreground } }
                            }
                        }
                    }
                    title_lbl := Label {
                        width: 70
                        height: Fit
                        text: "Thinking"
                        draw_text +: { color: theme.color_card_foreground }
                    }
                    summary := View {
                        width: Fill
                        height: 20
                        align: Align{y: 0.5}
                        clip_x: true
                        preview_lbl := mod.components.ClippedLabel {
                            draw_text +: { color: theme.color_muted_foreground }
                        }
                    }
                }
                body := RoundedView {
                    width: Fill
                    height: Fit
                    padding: Inset{left: 30 top: 5 right: 24 bottom: 8}
                    draw_bg +: {
                        color: theme.color_transparent
                        border_size: 0.0
                    }
                    md := mod.components.ChatMarkdown {}
                }
            }

            SubagentMsg := #(ToolFoldHeader::register_widget(vm)) {
                width: Fill
                height: Fit
                flow: Down
                body_walk: Walk{width: Fill, height: Fit}
                margin: Inset{top: 4 bottom: 2 left: 20 right: 24}
                opened: 0.0
                animator +: { active: { default: @off } }
                header := mod.components.ActivityHeader {
                    height: 26
                    title_lbl := Label { width: 92, text: "Agent tasks" }
                    icon_tile := View {
                        width: 20
                        height: 20
                        align: Align{x: 0.5 y: 0.5}
                        icon_stack := View {
                            width: 14
                            height: 14
                            flow: Overlay
                            icon_generic := mod.components.ActivitySvgIcon { visible: false }
                            icon_subagent := mod.components.ActivitySvgIcon { visible: true }
                        }
                    }
                    summary := View {
                        width: Fill
                        height: 20
                        flow: Right
                        spacing: 7
                        align: Align{y: 0.5}
                        clip_x: true
                        preview_lbl := mod.components.ClippedLabel {
                            width: Fit
                            draw_text +: { color: theme.color_muted_foreground }
                        }
                        status_indicator := ActivityStatusIndicator {}
                    }
                }
                body := RoundedView {
                    width: Fill
                    height: Fit
                    padding: Inset{left: 30 top: 4 right: 18 bottom: 8}
                    flow: Down
                    spacing: 6
                    draw_bg +: {
                        color: theme.color_transparent
                        border_size: 0.0
                    }
                    rail := #(SubagentRail::register_widget(vm)) {
                        width: Fill
                        height: Fit
                        flow: Down
                        spacing: 7
                        draw_bg +: { color: theme.color_transparent }
                        row_template: #(ToolFoldHeader::register_widget(vm)) {
                            width: Fill
                            height: Fit
                            flow: Down
                            body_walk: Walk{width: Fill, height: Fit}
                            margin: Inset{top: 4 bottom: 2 left: 20 right: 24}
                            opened: 0.0
                            animator +: { active: { default: @off } }
                            header := mod.components.ActivityHeader {
                                height: 28
                                padding: Inset{left: 3 top: 0 right: 4 bottom: 0}
                                title_lbl := Label {
                                    width: 82
                                    height: 20
                                    padding: 0
                                    align: Align{y: 0.5}
                                    draw_text +: {
                                        color: theme.color_foreground
                                        text_style: theme.font_regular { font_size: 12.0 }
                                    }
                                }
                                summary := View {
                                    width: Fill
                                    height: 20
                                    flow: Right
                                    spacing: 7
                                    align: Align{y: 0.5}
                                    clip_x: true
                                    preview_lbl := mod.components.ClippedLabel {
                                        width: Fill
                                        height: 20
                                        padding: 0
                                        align: Align{y: 0.5}
                                        draw_text +: { color: theme.color_muted_foreground }
                                    }
                                    status_lbl := Label {
                                        width: Fit
                                        height: 20
                                        align: Align{y: 0.5}
                                        padding: 0
                                        draw_text +: {
                                            color: theme.color_muted_foreground
                                            text_style +: { font_size: 9.0 }
                                        }
                                    }
                                    status_indicator := ActivityStatusIndicator {
                                        height: 20
                                        align: Align{y: 0.5}
                                    }
                                    resume_btn := Button {
                                        width: Fit
                                        height: 20
                                        text: "Resume"
                                        padding: Inset{left: 6 right: 6}
                                        visible: false
                                        draw_bg +: {
                                            color: theme.color_secondary
                                            color_hover: theme.color_primary
                                            color_focus: theme.color_primary
                                            color_down: theme.color_primary
                                            border_radius: 5.0
                                        }
                                        draw_text +: { color: theme.color_foreground text_style +: { font_size: 9.0 } }
                                    }
                                    abort_btn := Button {
                                        width: Fit
                                        height: 20
                                        text: "Abort"
                                        padding: Inset{left: 6 right: 6}
                                        visible: false
                                        draw_bg +: {
                                            color: theme.color_secondary
                                            color_hover: theme.color_destructive
                                            color_focus: theme.color_destructive
                                            color_down: theme.color_destructive
                                            border_radius: 5.0
                                        }
                                        draw_text +: { color: theme.color_foreground text_style +: { font_size: 9.0 } }
                                    }
                                }
                            }
                            body := RoundedView {
                                width: Fill
                                height: Fit
                                padding: Inset{left: 30 top: 2 right: 18 bottom: 6}
                                draw_bg +: {
                                    color: theme.color_transparent
                                    border_size: 0.0
                                }
                                working_detail := View {
                                    width: Fill
                                    height: 18
                                    visible: false
                                    flow: Right
                                    spacing: 7
                                    align: Align{y: 0.5}
                                    working_loader := ActivityLoader {
                                        width: 18
                                        height: 10
                                        draw_bg +: { dot_radius: 1.0 speed: 3.6 }
                                    }
                                    working_lbl := Label {
                                        text: "Working..."
                                        draw_text +: { color: theme.color_muted_foreground text_style +: { font_size: 9.0 } }
                                    }
                                }
                                detail_md := mod.components.ChatMarkdown {}
                            }
                        }
                    }
                }
            }

            ToolMsg := #(ToolFoldHeader::register_widget(vm)) {
                width: Fill
                height: Fit
                flow: Down
                body_walk: Walk{width: Fill, height: Fit}
                margin: Inset{top: 4 bottom: 2 left: 20 right: 24}
                opened: 0.0
                animator +: { active: { default: @off } }
                header := mod.components.ActivityHeader {
                    height: 26
                    title_lbl := Label { width: 86 }
                    summary := View {
                        width: Fill
                        height: 20
                        flow: Right
                        spacing: 7
                        align: Align{y: 0.5}
                        clip_x: true
                        preview_lbl := mod.components.CodeLabel {
                            width: Fit{max: FitBound.Abs(180)}
                            max_lines: 1
                            text_overflow: Ellipsis
                            draw_text +: {
                                color: theme.color_foreground
                                text_style +: { font_size: 9.0 }
                            }
                        }
                        result_preview_lbl := mod.components.CodeLabel {
                            width: Fill
                            visible: false
                            max_lines: 1
                            text_overflow: Ellipsis
                            draw_text +: { color: theme.color_muted_foreground }
                        }
                        result_meta_header_lbl := mod.components.ClippedLabel {
                            width: Fit
                            visible: false
                            draw_text +: {
                                color: theme.color_primary
                                text_style +: { font_size: 8.0 }
                            }
                        }

                        status_indicator := ActivityStatusIndicator {}
                    }
                }
                body := RoundedView {
                    width: Fill
                    height: Fit
                    padding: Inset{left: 30 top: 2 right: 18 bottom: 6}
                    flow: Down
                    spacing: 4
                    draw_bg +: {
                        color: theme.color_transparent
                        border_size: 0.0
                    }
                    details_row := View {
                        width: Fill
                        height: Fit
                        flow: Right
                        spacing: 8
                        meta_lbl := mod.components.CodeLabel {
                            draw_text +: { color: theme.color_primary }
                        }
                        result_meta_lbl := mod.components.CodeLabel {
                            draw_text +: { color: theme.color_muted_foreground }
                        }
                    }
                    args_section := ToolSection {
                        section_label +: { text: "INPUT" }
                    }
                    result_section := ToolSection {
                        section_label +: { text: "OUTPUT" }
                        content_lbl +: { draw_text +: { color: theme.color_card_foreground } }
                    }
                }
            }
        }

        jump_to_latest_layer := View {
            width: Fill
            height: Fill
            flow: Overlay
            align: Align{x: 1.0 y: 1.0}
            visible: false

            jump_to_latest_btn := mod.components.IconButton {
                width: 32
                height: 32
                margin: Inset{right: 16 bottom: 12}
                icon_walk: Walk{width: 17 height: 17 margin: 0}
                draw_icon +: {
                    svg: crate_resource("self:resources/icons/jump-latest.svg")
                    color: theme.color_muted_foreground
                    color_hover: theme.color_foreground
                    color_focus: theme.color_foreground
                    color_down: theme.color_primary_foreground
                }
                draw_bg +: {
                    color: theme.color_card
                    color_hover: theme.color_secondary
                    color_focus: theme.color_secondary
                    color_down: theme.color_input
                    border_color: theme.color_border
                    border_color_hover: theme.color_border
                    border_color_focus: theme.color_border
                    border_color_down: theme.color_border
                    border_size: 1.0
                    border_radius: 18.0
                }
            }

            jump_to_latest_hint := RoundedView {
                width: 126
                height: 28
                margin: Inset{right: 16 bottom: 52}
                visible: false
                padding: Inset{left: 9 right: 9 top: 5 bottom: 5}
                draw_bg +: {
                    color: theme.color_popover
                    border_color: theme.color_border
                    border_size: 1.0
                    border_radius: 7.0
                }
                lbl := Label {
                    width: Fill
                    height: Fit
                    text: "Jump to latest"
                    draw_text +: {
                        color: theme.color_card_foreground
                        text_style: theme.font_regular {font_size: 9.0}
                    }
                }
            }
        }
    }

    // -------------------------------------------------------------------
    let SessionList = #(SessionList::register_widget(vm)) {
        width: Fill
        height: Fill
        flow: Down
        spacing: 0

        mod.components.ProjectHeaderActiveBase = mod.components.ProjectHeaderBase {
            draw_bg +: {
                color: theme.color_background
                color_hover: theme.color_card
                border_color: theme.color_border
                border_size: 1.0
            }
            project_toggle_surface +: {
                folder_icon +: { draw_icon +: { color: theme.color_primary } }
                name_lbl +: { draw_text +: { color: theme.color_foreground } }
            }
        }

        fixed_header_slot := View {
            width: Fill
            height: 44
            flow: Overlay
            clip_x: true
            clip_y: true

            fixed_project_header := mod.components.ProjectHeaderBase {}
            fixed_project_header_active := mod.components.ProjectHeaderActiveBase {
                visible: false
            }
        }

        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: true
            scroll_bar: mod.widgets.ScrollBar {
                bar_size: 10.0
                bar_side_margin: 3.0
                min_handle_size: 30.0
                draw_bg +: {
                    size: 5.0
                    border_size: 0.0
                    color: theme.color_transparent
                    color_hover: theme.color_transparent
                    color_drag: theme.color_transparent
                    border_color: theme.color_transparent
                    border_color_hover: theme.color_transparent
                    border_color_drag: theme.color_transparent
                }
            }

            FixedHeaderSpacer := View {
                width: Fill
                height: 1
                show_bg: true
                draw_bg +: { color: theme.color_transparent }
            }

            ProjectHeader := mod.components.ProjectHeaderBase {}

            ProjectHeaderActive := mod.components.ProjectHeaderActiveBase {}

            SessionRow := SessionRowBase {}

            SessionRowLast := SessionRowBase {
                draw_bg +: { tree_last: 1.0 }
            }

            SessionOverflow := View {
                width: Fill
                height: 28
                padding: Inset{left: 43 right: 10}
                align: Align{y: 0.5}
                overflow_btn := Button {
                    width: Fit
                    height: 24
                    padding: 0
                    spacing: 0
                    text: "Show more"
                    align: Align{x: 0.0 y: 0.5}
                    draw_bg +: {
                        color: theme.color_transparent
                        color_hover: theme.color_transparent
                        color_focus: theme.color_transparent
                        color_down: theme.color_transparent
                        border_color: theme.color_transparent
                        border_color_hover: theme.color_transparent
                        border_color_focus: theme.color_transparent
                        border_color_down: theme.color_transparent
                        border_size: 0.0
                    }
                    draw_text +: {
                        color: theme.color_muted_foreground
                        color_hover: theme.color_primary
                        color_focus: theme.color_primary
                        color_down: theme.color_foreground
                        text_style +: { font_size: 9.5 }
                    }
                }
            }

            EmptyRow := EmptyRowBase {
                padding: Inset{left: 43 top: 4 right: 10 bottom: 8}
                lbl +: {
                    text: "No sessions yet"
                    draw_text +: { color: theme.color_muted_foreground text_style +: { font_size: 9.5 } }
                }
            }
        }
    }

    let SettingsActionButton = mod.components.SecondaryActionButton

    let ProvidersModal = #(ProviderSettingsModal::register_widget(vm)) {
        width: Fill
        height: Fill
        flow: Overlay
        align: Align{x: 0.5 y: 0.5}

        modal_backdrop := mod.components.ModalDialogBackdrop {}

        modal_card := mod.components.ModalDialogCard {

            settings_nav := View {
                width: 180
                height: Fill
                flow: Down
                padding: Inset{left: 16 top: 24 right: 12 bottom: 20}
                spacing: 8
                draw_bg +: {
                    color: theme.color_background
                    border_color: theme.color_card
                    border_size: 1.0
                }

                providers_category_lbl := mod.components.CategoryHeaderLabel {
                    margin: Inset{bottom: 4}
                    text: "PROVIDERS"
                }

                settings_nav_google_btn := mod.components.NavButton {
                    text: "Google Antigravity"
                }

                settings_nav_openai_btn := mod.components.NavButton {
                    text: "OpenAI / ChatGPT"
                }

                settings_nav_opencode_btn := mod.components.NavButton {
                    text: "OpenCode Go"
                }

                advanced_category_lbl := mod.components.CategoryHeaderLabel {
                    margin: Inset{top: 18 bottom: 4}
                    text: "ADVANCED"
                }

                settings_nav_capabilities_btn := mod.components.NavButton {
                    text: "WASI Extensions"
                }

                settings_nav_skills_btn := mod.components.NavButton {
                    text: "Skills"
                }

                settings_nav_mcp_btn := mod.components.NavButton {
                    text: "MCP Servers"
                }

                settings_nav_acp_btn := mod.components.NavButton {
                    text: "ACP Agents"
                }

                settings_nav_about_btn := mod.components.NavButton {
                    text: "About"
                }
            }

            settings_content := View {
                width: Fill
                height: Fill
                flow: Down
                padding: Inset{left: 26 top: 20 right: 24 bottom: 22}
                spacing: 14

                modal_header := View {
                    width: Fill
                    height: Fit
                    flow: Right
                    align: Align{y: 0.5}

                    modal_title := Label {
                        width: Fill
                        height: Fit
                        text: "Provider Settings"
                        draw_text +: {
                            color: theme.color_foreground
                            text_style: theme.font_bold { font_size: 14.0 }
                        }
                    }

                    close_modal_btn := Button {
                        width: 26
                        height: 26
                        padding: 0
                        spacing: 0
                        text: ""
                        align: Align{x: 0.5 y: 0.5}
                        icon_walk: Walk{width: 12 height: 12}
                        draw_bg +: {
                            color: theme.color_transparent
                            color_hover: theme.color_card
                            color_focus: theme.color_card
                            color_down: theme.color_secondary
                            border_color: theme.color_transparent
                            border_color_hover: theme.color_transparent
                            border_color_focus: theme.color_transparent
                            border_color_down: theme.color_transparent
                            border_size: 0.0
                            border_radius: 6.0
                        }
                        draw_icon +: {
                            svg: crate_resource("self:resources/icons/close.svg")
                            color: theme.color_muted_foreground
                            color_hover: theme.color_foreground
                            color_focus: theme.color_foreground
                            color_down: theme.color_primary_foreground
                        }
                    }
                }

                google_antigravity_page := View {
                    width: Fill
                    height: Fill
                    flow: Down
                    spacing: 14

                    google_page_title := mod.components.PageTitleLabel {
                        text: "Google Antigravity"
                    }

                    google_page_desc := mod.components.PageDescriptionLabel {
                        text: "Connect your AI model providers to use them in Threadlane."
                    }

                    antigravity_card := mod.components.ProviderCard {

                        ag_header := mod.components.ProviderCardHeader {

                            ag_title := mod.components.ProviderCardTitle {
                                text: "Google Antigravity"
                            }

                            antigravity_status_lbl := mod.components.ProviderCardStatus {
                                text: "Not Connected"
                            }
                        }

                        ag_desc := mod.components.ProviderCardDescription {
                            text: "Cloud Code Assist, Gemini 3.6 Flash / Pro via Google OAuth PKCE"
                        }

                        ag_actions := mod.components.ProviderCardActions {

                            antigravity_login_btn := mod.components.PrimaryActionButton {
                                text: "Sign in with Google"
                            }

                            antigravity_doctor_btn := mod.components.SecondaryActionButton {
                                text: "Run Health Check"
                            }
                        }
                    }
                }

                openai_page := View {
                    width: Fill
                    height: Fill
                    flow: Down
                    spacing: 14
                    visible: false

                    openai_page_title := mod.components.PageTitleLabel {
                        text: "OpenAI / ChatGPT"
                    }

                    openai_page_desc := mod.components.PageDescriptionLabel {
                        text: "Connect your AI model providers to use them in Threadlane."
                    }

                    openai_card := mod.components.ProviderCard {

                        oa_header := mod.components.ProviderCardHeader {

                            oa_title := mod.components.ProviderCardTitle {
                                text: "OpenAI / ChatGPT"
                            }

                            openai_status_lbl := mod.components.ProviderCardStatus {
                                text: "Not Connected"
                            }
                        }

                        oa_desc := mod.components.ProviderCardDescription {
                            text: "GPT-4o, Codex, and OpenAI models via ChatGPT OAuth or API key"
                        }

                        oa_actions := mod.components.ProviderCardActions {

                            openai_login_btn := Button {
                                width: Fit
                                height: 28
                                padding: Inset{left: 12 right: 12 top: 4 bottom: 4}
                                text: "Sign in with ChatGPT"
                                draw_bg +: {
                                    color: theme.color_success
                                    color_hover: theme.color_success
                                    color_down: theme.color_success
                                    border_radius: 6.0
                                }
                                draw_text +: {
                                    color: theme.color_primary_foreground
                                    text_style: theme.font_bold { font_size: 9.5 }
                                }
                            }
                        }
                    }
                }

                opencode_page := View {
                    width: Fill
                    height: Fill
                    flow: Down
                    spacing: 14
                    visible: false

                    opencode_page_title := mod.components.PageTitleLabel {
                        text: "OpenCode Go"
                    }

                    opencode_page_desc := mod.components.PageDescriptionLabel {
                        text: "Use OpenCode Go models through the OpenCode Zen API."
                    }

                    opencode_go_card := mod.components.ProviderCard {
                        oc_header := mod.components.ProviderCardHeader {
                            oc_title := mod.components.ProviderCardTitle {
                                text: "OpenCode Go"
                            }
                            opencode_status_lbl := mod.components.ProviderCardStatus {
                                text: "Not Connected"
                            }
                        }

                        oc_desc := mod.components.ProviderCardDescription {
                            text: "Curated open-source and proprietary models via OpenCode Zen"
                        }

                        opencode_api_key_input := TextInput {
                            width: Fill
                            height: 32
                            empty_text: "OpenCode API key (or OPENCODE_API_KEY)"
                        }

                        oc_actions := mod.components.ProviderCardActions {
                            opencode_save_btn := mod.components.PrimaryActionButton {
                                text: "Save API key"
                            }
                            opencode_clear_btn := mod.components.SecondaryActionButton {
                                text: "Clear"
                            }
                        }
                    }
                }

                capabilities_page := View {
                    width: Fill
                    height: Fill
                    flow: Down
                    spacing: 12
                    visible: false

                    capability_header := View {
                        width: Fill
                        height: 28
                        flow: Right
                        spacing: 6
                        align: Align{y: 0.5}

                        capability_page_title := mod.components.PageTitleLabel {
                            height: 28
                            padding: 0
                            align: Align{y: 0.5}
                            text: "WASI Extensions"
                        }

                        capability_install_scope_lbl := Label {
                            width: Fit
                            height: Fit
                            padding: 0
                            text: "Install to"
                            draw_text +: {
                                color: theme.color_muted_foreground
                                text_style +: { font_size: 8.75 }
                            }
                        }
                        capability_scope_global_btn := mod.components.ScopeButton { text: "Global" }
                        capability_scope_project_btn := mod.components.SelectedScopeButton { text: "Project" }
                        capability_add_btn := mod.components.IconButton {
                            draw_icon +: {
                                svg: crate_resource("self:resources/icons/plus.svg")
                            }
                        }
                        capability_refresh_btn := mod.components.IconButton {
                            draw_icon +: {
                                svg: crate_resource("self:resources/icons/refresh.svg")
                            }
                        }
                    }

                    capability_page_desc := mod.components.PageDescriptionLabel {
                        padding: 0
                        text: "Choose a compiled .wasm file. Threadlane never runs Cargo or build scripts."
                    }

                    capability_build_command := mod.components.CodeLabel {
                        padding: 0
                        text: "cargo build --target wasm32-wasip1 --release"
                    }

                    capability_list := PortalList {
                        width: Fill
                        height: Fill
                        flow: Down
                        spacing: 8

                        ExtensionRow := mod.components.CapabilityRowWithRemove {}

                        EmptyRow := mod.components.CapabilityEmptyRow {
                            empty_lbl: { text: "No WASI extensions found." }
                        }
                    }

                    capability_status_lbl := mod.components.SettingsStatusLabel {}
                }

                skills_page := View {
                    width: Fill
                    height: Fill
                    flow: Down
                    spacing: 12
                    visible: false

                    skill_header := View {
                        width: Fill
                        height: 28
                        flow: Right
                        spacing: 6
                        align: Align{y: 0.5}

                        skill_page_title := mod.components.PageTitleLabel {
                            height: 28
                            padding: 0
                            align: Align{y: 0.5}
                            text: "Skills"
                        }

                        skill_refresh_btn := mod.components.IconButton {
                            draw_icon +: {
                                svg: crate_resource("self:resources/icons/refresh.svg")
                            }
                        }
                    }

                    skill_page_desc := mod.components.PageDescriptionLabel {
                        padding: 0
                        text: "Enable or disable discovered skills for this project. Disabled skills are hidden from the composer and the model."
                    }

                    skill_list := PortalList {
                        width: Fill
                        height: Fill
                        flow: Down
                        spacing: 8

                        SkillRow := mod.components.CapabilityRowBase {}

                        SkillEmptyRow := mod.components.CapabilityEmptyRow {
                            empty_lbl: { text: "No skills found." }
                        }
                    }

                    skill_status_lbl := mod.components.SettingsStatusLabel {}
                }

                mcp_page := View {
                    width: Fill
                    height: Fill
                    flow: Down
                    spacing: 12
                    visible: false

                    mcp_header := View {
                        width: Fill
                        height: 28
                        flow: Right
                        spacing: 6
                        align: Align{y: 0.5}

                        mcp_page_title := mod.components.PageTitleLabel {
                            height: 28
                            padding: 0
                            align: Align{y: 0.5}
                            text: "MCP Servers"
                        }

                        mcp_scope_global_btn := mod.components.ScopeButton { text: "Global" }
                        mcp_scope_project_btn := mod.components.SelectedScopeButton { text: "Project" }
                        mcp_refresh_btn := mod.components.IconButton {
                            draw_icon +: {
                                svg: crate_resource("self:resources/icons/refresh.svg")
                            }
                        }
                    }

                    mcp_page_desc := mod.components.PageDescriptionLabel {
                        padding: 0
                        text: "Model Context Protocol servers providing external tools over stdio."
                    }

                    mcp_add_card := mod.components.AddServerCard {
                        add_title +: { text: "Add MCP Server" }
                        add_inputs +: {
                            mcp_name_input := mod.components.ThemedTextInput {
                                width: 140
                                height: 30
                                empty_text: "Name (e.g. fs)"
                            }
                            mcp_command_input := mod.components.ThemedTextInput {
                                width: Fill
                                height: 30
                                empty_text: "Command (e.g. npx -y @modelcontextprotocol/server-filesystem /path)"
                            }
                            mcp_submit_add_btn := mod.components.PrimaryActionButton {
                                height: 30
                                text: "Add Server"
                            }
                        }
                    }

                    mcp_list := PortalList {
                        width: Fill
                        height: Fill
                        flow: Down
                        spacing: 8

                        McpRow := mod.components.CapabilityRowWithRemove {}

                        McpEmptyRow := mod.components.CapabilityEmptyRow {
                            empty_lbl: { text: "No MCP servers configured." }
                        }
                    }

                    mcp_status_lbl := mod.components.SettingsStatusLabel {}
                }

                acp_page := View {
                    width: Fill
                    height: Fill
                    flow: Down
                    spacing: 12
                    visible: false

                    acp_header := View {
                        width: Fill
                        height: 28
                        flow: Right
                        spacing: 6
                        align: Align{y: 0.5}

                        acp_page_title := Label {
                            width: Fill
                            height: 28
                            padding: 0
                            align: Align{y: 0.5}
                            text: "ACP Agents"
                            draw_text +: {
                                color: theme.color_foreground
                                text_style: theme.font_bold { font_size: 18.0 }
                            }
                        }

                        acp_scope_global_btn := mod.components.ScopeButton { text: "Global" }
                        acp_scope_project_btn := mod.components.SelectedScopeButton { text: "Project" }
                        acp_refresh_btn := mod.components.IconButton {
                            draw_icon +: {
                                svg: crate_resource("self:resources/icons/refresh.svg")
                            }
                        }
                    }

                    acp_page_desc := Label {
                        width: Fill
                        height: Fit
                        padding: 0
                        text: "External coding agents that speak the Agent Client Protocol over stdio."
                        draw_text +: {
                            color: theme.color_muted_foreground
                            text_style +: { font_size: 10.0 }
                        }
                    }

                    acp_add_card := mod.components.AddServerCard {
                        add_title +: { text: "Add ACP Agent" }
                        add_inputs +: {
                            acp_name_input := mod.components.ThemedTextInput {
                                width: 140
                                height: 30
                                empty_text: "Name (e.g. Gemini)"
                            }
                            acp_command_input := mod.components.ThemedTextInput {
                                width: Fill
                                height: 30
                                empty_text: "Command (e.g. gemini --experimental-acp)"
                            }
                            acp_submit_add_btn := mod.components.PrimaryActionButton {
                                height: 30
                                text: "Add Agent"
                            }
                        }
                    }

                    acp_list := PortalList {
                        width: Fill
                        height: Fill
                        flow: Down
                        spacing: 8

                        AcpRow := mod.components.CapabilityRowWithRemove {}

                        AcpEmptyRow := mod.components.CapabilityEmptyRow {
                            empty_lbl: { text: "No ACP agents configured." }
                        }
                    }

                    acp_status_lbl := mod.components.SettingsStatusLabel {}
                }

                about_page := View {
                    width: Fill
                    height: Fill
                    flow: Down
                    spacing: 14
                    visible: false

                    about_title_row := View {
                        width: Fill
                        height: Fit
                        flow: Right
                        spacing: 8
                        align: Align{y: 0.5}

                        about_title_icon := Icon {
                            width: 28
                            height: 28
                            icon_walk: Walk{width: 28 height: 28}
                            draw_icon +: {
                                svg: crate_resource("self:resources/icons/logo.svg")
                                color: theme.color_foreground
                            }
                        }

                        about_title_lbl := mod.components.PageTitleLabel {
                            text: "Threadlane"
                        }
                        }

                    about_version_lbl := Label {
                        width: Fill
                        height: Fit
                        text: "Version"
                        draw_text +: {
                            color: theme.color_primary
                            text_style: theme.font_bold { font_size: 10.0 }
                        }
                    }

                    about_description_lbl := Label {
                        width: Fill
                        height: Fit
                        text: "Threadlane is a focused workspace for building software with AI coding agents."
                        draw_text +: {
                            color: theme.color_foreground
                            text_style +: { font_size: 11.0 }
                        }
                    }

                    about_detail_lbl := mod.components.PageDescriptionLabel {
                        text: "Keep projects, sessions, and provider connections together in one calm, native desktop app."
                    }
                }
            }
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(1280, 768)
                window.title: "Threadlane"
                pass.clear_color: theme.color_background
                body +: {
                    window_body := View {
                        width: Fill
                        height: Fill
                        flow: Overlay

                        dock := DockFlat {
                        width: Fill
                        height: Fill
                        padding: 0

                        round_corner +: {
                            border_radius: 0.0
                        }

                        splitter: Splitter {
                            size: 6.0
                            draw_bg +: {
                                color: uniform(theme.color_card)
                                color_hover: uniform(theme.color_primary)
                                color_drag: uniform(theme.color_primary)

                                pixel: fn() {
                                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                    sdf.clear(theme.color_transparent)
                                    let line_color = mix(
                                        self.color
                                        mix(self.color_hover, self.color_drag, self.drag)
                                        self.hover
                                    )
                                    if self.is_vertical > 0.5 {
                                        sdf.rect(0.0, self.rect_size.y * 0.5 - 0.5, self.rect_size.x, 1.0)
                                    } else {
                                        sdf.rect(self.rect_size.x * 0.5 - 0.5, 0.0, 1.0, self.rect_size.y)
                                    }
                                    sdf.fill(line_color)
                                    return sdf.result
                                }
                            }
                        }

                        root := DockSplitter {
                            axis: SplitterAxis.Horizontal
                            align: SplitterAlign.FromA(250.0)
                            a: @sessions_tabs
                            b: @workspace_tabs
                        }

                        sessions_tabs := DockTabs {
                            tabs: [@sessions_tab]
                            selected: 0
                            closable: false
                            hide_tab_bar: true
                        }

                        workspace_tabs := DockTabs {
                            tabs: [@workspace_tab]
                            selected: 0
                            closable: false
                            hide_tab_bar: true
                        }

                        sessions_tab := DockTab {
                            name: "Sessions"
                            template: @PermanentTab
                            kind: @SessionsDock
                        }

                        workspace_tab := DockTab {
                            name: "Workspace"
                            template: @PermanentTab
                            kind: @WorkspaceDock
                        }

                        SessionsDock := View {
                            width: Fill
                            height: Fill
                            flow: Down
                            spacing: 0
                            padding: Inset{left: 8 top: 8 right: 8 bottom: 10}
                            show_bg: true
                            draw_bg +: {
                                color: theme.color_secondary
                                pixel: fn() {
                                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                    sdf.clear(theme.color_transparent)
                                    sdf.rect(self.rect_size.x - 1.0, 0.0, 1.0, self.rect_size.y)
                                    return sdf.fill(self.color)
                                }
                            }

                            sidebar_brand := View {
                                width: Fill
                                height: 44
                                flow: Right
                                align: Align{y: 0.5}
                                padding: Inset{left: 7 right: 5 bottom: 6}
                                spacing: 7
                                sidebar_brand_icon := Icon {
                                    width: 16
                                    height: 16
                                    icon_walk: Walk{width: 16 height: 16}
                                    draw_icon +: {
                                        svg: crate_resource("self:resources/icons/logo.svg")
                                        color: theme.color_foreground
                                    }
                                }
                                sidebar_brand_label := Label {
                                    width: Fit
                                    height: Fit
                                    text: "Threadlane"
                                    draw_text +: {
                                        color: theme.color_foreground
                                        text_style: theme.font_bold { font_size: 14.0 }
                                    }
                                }
                                sidebar_brand_spacer := mod.components.FlexSpacer {}
                                left_sidebar_toggle_btn := mod.components.ToolbarIconButton {
                                    draw_icon +: {
                                        svg: crate_resource("self:resources/icons/sidebar_left.svg")
                                    }
                                }
                                settings_btn := mod.components.ToolbarIconButton {
                                    draw_icon +: {
                                        svg: crate_resource("self:resources/icons/settings.svg")
                                    }
                                }
                            }
                            sidebar_brand_divider := View {
                                width: Fill
                                height: 1
                                margin: Inset{left: 6 right: 6 bottom: 4}
                                show_bg: true
                                draw_bg +: { color: theme.color_input }
                            }
                            sidebar_search := TextInput {
                                width: Fill
                                height: 32
                                margin: Inset{left: 3 right: 3 bottom: 8}
                                padding: Inset{left: 10 right: 10}
                                empty_text: "Search projects and sessions"
                                draw_bg +: {
                                    color: theme.color_input
                                    color_focus: theme.color_input
                                    border_color: theme.color_secondary
                                    border_color_focus: theme.color_primary
                                    border_radius: 7.0
                                    border_size: 1.0
                                }
                                draw_text +: {
                                    color: theme.color_foreground
                                    color_empty: theme.color_muted_foreground
                                }
                            }

                            projects_header := mod.components.SectionHeader {
                                section_label +: { text: "PROJECTS" }
                                add_project_btn := mod.components.SidebarComposeButton {}
                            }
                            session_list := SessionList { height: Fill }
                            session_context_menu := SessionContextMenu {}
                            providers_modal := ProvidersModal {}
                            update_action_row := View {
                                width: Fill
                                height: Fit
                                visible: false
                                margin: Inset{left: 3 top: 8 right: 3}

                                update_btn := Button {
                                    width: Fill
                                    height: 34
                                    padding: Inset{left: 10 right: 10 top: 6 bottom: 6}
                                    text: "Update Threadlane"
                                    draw_bg +: {
                                        color: theme.color_background
                                        color_hover: theme.color_background
                                        color_focus: theme.color_background
                                        color_down: theme.color_card
                                        border_color: theme.color_secondary
                                        border_color_hover: theme.color_success
                                        border_color_focus: theme.color_success
                                        border_color_down: theme.color_success
                                        border_radius: 8.0
                                        border_size: 1.0
                                    }
                                    draw_text +: {
                                        color: theme.color_success
                                        color_hover: theme.color_success
                                        color_focus: theme.color_success
                                        color_down: theme.color_primary_foreground
                                        text_style: theme.font_bold { font_size: 9.0 }
                                    }
                                }
                            }
                        }

                        WorkspaceDock := View {
                            width: Fill
                            height: Fill
                            flow: Overlay

                            workspace_content := View {
                                width: Fill
                                height: Fill
                                flow: Down
                                spacing: 5
                                padding: Inset{left: 10 top: 8 right: 12 bottom: 10}

                                header := PanelHeader {
                            spacing: 8
                            padding: Inset{left: 4 top: 1 right: 2 bottom: 2}

                            left_sidebar_expand_btn := mod.components.ToolbarIconButton {
                                visible: false
                                draw_icon +: {
                                    svg: crate_resource("self:resources/icons/sidebar_left.svg")
                                }
                            }

                            project_identity := View {
                                width: Fill
                                height: Fit
                                flow: Down
                                spacing: 0
                                clip_x: true

                                project_title_row := View {
                                    width: Fill
                                    height: 18
                                    flow: Right
                                    spacing: 7
                                    align: Align{y: 0.5}

                                    project_icon := Icon {
                                        width: 14
                                        height: 14
                                        icon_walk: Walk{width: 12 height: 12}
                                        draw_icon +: {
                                            svg: crate_resource("self:resources/icons/folder.svg")
                                            color: theme.color_primary
                                        }
                                    }
                                    project_name_label := mod.components.ClippedLabel {
                                        height: 18
                                        padding: 0
                                        align: Align{y: 0.6}
                                        draw_text +: {
                                            color: theme.color_primary_foreground
                                            text_style: theme.font_bold { font_size: 12.0 }
                                        }
                                    }
                                }
                                workspace_label := mod.components.ClippedLabel {
                                    height: 14
                                    padding: 0
                                    align: Align{y: 0.5}
                                    draw_text +: {
                                        color: theme.color_muted_foreground
                                        text_style +: { font_size: 9.0 }
                                    }
                                }
                            }

                            terminal_header_btn := mod.components.ToolbarIconButton {
                                draw_icon +: { svg: crate_resource("self:resources/icons/panel-down.svg") }
                            }

                            right_sidebar_toggle_btn := mod.components.ToolbarIconButton {
                                visible: false
                                draw_icon +: {
                                    svg: crate_resource("self:resources/icons/sidebar_right.svg")
                                }
                            }

                            caps_btn := mod.components.HeaderChipButton {
                                padding: Inset{left: 8 right: 9 top: 4 bottom: 4}
                                text: "Tools"
                                icon_walk: Walk{width: 13 height: 13 margin: Inset{right: 2}}
                                draw_icon +: {
                                    svg: crate_resource("self:resources/icons/skill.svg")
                                }
                            }

                            status_pill := StatusPill {}
                        }

                        update_notice := mod.components.NoticeBanner {
                            update_notice_visual := View {
                                width: 18
                                height: 18
                                align: Align{x: 0.5 y: 0.5}
                                update_notice_loader := ActivityLoader {
                                    width: 18
                                    height: 11
                                    visible: false
                                    draw_bg +: { dot_radius: 1.05 speed: 3.0 }
                                }
                                update_notice_available_dot := mod.components.StatusDot {
                                    draw_bg +: { color: theme.color_primary }
                                }
                                update_notice_ready_dot := mod.components.StatusDot {
                                    draw_bg +: { color: theme.color_success }
                                }
                                update_notice_error_dot := mod.components.StatusDot {
                                    draw_bg +: { color: theme.color_destructive }
                                }
                            }

                            update_notice_title := Label {
                                width: Fit
                                height: Fit
                                text: ""
                                draw_text +: {
                                    color: theme.color_primary_foreground
                                    text_style: theme.font_bold { font_size: 9.5 }
                                }
                            }
                            update_notice_detail := mod.components.ClippedLabel {
                                draw_text +: {
                                    color: theme.color_primary
                                    text_style +: { font_size: 8.5 }
                                }
                            }
                        }

                        auth_row := AuthRow {}

                        content_row := View {
                            width: Fill
                            height: Fill
                            flow: Right
                            spacing: 10

                            chat_column := View {
                                width: Fill
                                height: Fill
                                flow: Down
                                spacing: 8

                            chat_panel := PanelSurface {
                                width: Fill
                                height: Fill
                                flow: Down
                                padding: Inset{left: 4 top: 6 right: 4 bottom: 6}
                                draw_bg +: {
                                    color: theme.color_background
                                    border_radius: 10.0
                                }
                                chat_list := ChatList {
                                    width: Fill
                                    height: Fill
                                }
                                chat_working_indicator := View {
                                    width: Fill
                                    height: 26
                                    margin: Inset{top: 8 bottom: 4}
                                    padding: Inset{left: 20}
                                    visible: false
                                    flow: Right
                                    spacing: 8
                                    align: Align{x: 0.0 y: 0.5}
                                    chat_working_spinner := ActivityLoader {
                                        width: 28
                                        height: 16
                                        draw_bg +: {
                                            dot_radius: 1.15
                                            speed: 8.0
                                        }
                                    }
                                    chat_working_label := mod.components.ClippedLabel {
                                        width: Fill
                                        draw_text +: {
                                            color: theme.color_muted_foreground
                                            text_style +: { font_size: 9.0 }
                                        }
                                    }
                                }
                            }

                            queued_message_preview := RoundedView {
                                width: Fill
                                height: Fit
                                visible: false
                                flow: Right
                                spacing: 6
                                padding: Inset{left: 12 right: 8 top: 8 bottom: 8}
                                align: Align{y: 0.5}
                                draw_bg +: {
                                    color: theme.color_secondary
                                    border_color: theme.color_border
                                    border_size: 1.0
                                    border_radius: 8.0
                                }

                                queued_message_text := mod.components.ClippedLabel {
                                    width: Fill
                                    height: Fit
                                    draw_text +: {
                                        color: theme.color_foreground
                                        text_style +: { font_size: 10.0 }
                                    }
                                }

                                queue_btn := mod.components.ComposerChip {
                                    text: "Queue"
                                }

                                steer_btn := mod.components.ComposerChip {
                                    text: "Steer"
                                    draw_bg +: {
                                        color: theme.color_primary
                                        color_hover: theme.color_primary
                                        color_down: theme.color_primary
                                        border_color: theme.color_transparent
                                        border_color_hover: theme.color_transparent
                                    }
                                    draw_text +: {
                                        color: theme.color_primary_foreground
                                        color_hover: theme.color_primary_foreground
                                        color_down: theme.color_primary_foreground
                                    }
                                }
                            }

                            input_bar := mod.components.ComposerSurface {
                                width: Fill
                                height: Fit
                                flow: Down
                                spacing: 4
                                padding: Inset{left: 9 top: 7 right: 7 bottom: 7}
                                new_batch: true
                                draw_bg +: {
                                    color: theme.color_card
                                    color_hover: theme.color_secondary
                                    color_focus: theme.color_background
                                    border_color: theme.color_border
                                    border_color_focus: theme.color_primary
                                    border_color_down: theme.color_primary
                                    border_color_error: theme.color_destructive
                                    border_size: 1.0
                                    border_radius: 11.0
                                }

                                attachment_row := View {
                                    width: Fill
                                    height: 24
                                    visible: false
                                    flow: Right
                                    spacing: 6
                                    clip_x: true

                                    attachment_chip_0 := mod.components.AttachmentChip { text: "" }
                                    attachment_chip_1 := mod.components.AttachmentChip { text: "" }
                                    attachment_chip_2 := mod.components.AttachmentChip { text: "" }
                                    attachment_chip_3 := mod.components.AttachmentChip { text: "" }
                                }

                                prompt_input := mod.components.ThreadlaneCommandTextInput {
                                    width: Fill
                                    height: Fit
                                    trigger: "/"
                                    inline_search: true
                                    color_focus: theme.color_background
                                    color_hover: theme.color_secondary

                                    persistent +: {
                                        width: Fill
                                        height: Fit
                                        center +: {
                                            width: Fill
                                            height: Fit
                                            text_input +: {
                                                width: Fill
                                                height: Fit{min: FitBound.Abs(56), max: FitBound.Abs(180)}
                                                margin: 0
                                                padding: Inset{left: 3 top: 6 right: 3 bottom: 6}
                                                is_multiline: true
                                                submit_on_enter: true
                                                empty_text: "Ask threadlane anything…"
                                                draw_bg +: {
                                                    color: theme.color_transparent
                                                    color_empty: theme.color_transparent
                                                    color_hover: theme.color_transparent
                                                    color_focus: theme.color_transparent
                                                    color_down: theme.color_transparent
                                                    border_color: theme.color_transparent
                                                    border_color_empty: theme.color_transparent
                                                    border_color_hover: theme.color_transparent
                                                    border_color_focus: theme.color_transparent
                                                    border_color_down: theme.color_transparent
                                                    border_size: 0.0
                                                }
                                                draw_text +: {
                                                    color: theme.color_foreground
                                                    color_hover: theme.color_foreground
                                                    color_focus: theme.color_foreground
                                                    color_empty: theme.color_muted_foreground
                                                    color_empty_hover: theme.color_muted_foreground
                                                    color_empty_focus: theme.color_muted_foreground
                                                    text_style +: {
                                                        font_size: 10.5
                                                        line_spacing: 1.35
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                composer_footer := View {
                                    width: Fill
                                    height: 30
                                    flow: Right
                                    spacing: 6
                                    align: Align{y: 0.5}
                                    clip_x: false
                                    clip_y: false

                                    composer_status := mod.components.ClippedLabel {
                                        width: Fit
                                        visible: false
                                        draw_text +: {
                                            color: theme.color_muted_foreground
                                            text_style +: { font_size: 8.5 }
                                        }
                                    }
                                    attach_btn := mod.components.IconButton {
                                        width: 30
                                        height: 28
                                        icon_walk: Walk{width: 14 height: 14}
                                        draw_icon +: {
                                            svg: crate_resource("self:resources/icons/attach.svg")
                                            color: theme.color_muted_foreground
                                            color_hover: theme.color_foreground
                                            color_down: theme.color_primary_foreground
                                        }
                                        draw_bg +: {
                                            color: theme.color_secondary
                                            color_hover: theme.color_secondary
                                            color_down: theme.color_input
                                            border_color: theme.color_border
                                            border_color_hover: theme.color_input
                                            border_size: 1.0
                                            border_radius: 6.0
                                        }
                                    }

                                    composer_hint := mod.components.ClippedLabel {
                                        text: "Enter sends · Shift+Enter adds a line"
                                        draw_text +: {
                                            color: theme.color_muted_foreground
                                            text_style +: { font_size: 8.5 }
                                        }
                                    }
                                    effort_picker := View {
                                        width: 92
                                        height: 28
                                        visible: false
                                        flow: Down
                                        clip_x: false
                                        clip_y: false

                                        effort_drop := EffortDropDown {
                                            labels: [
                                                "Off",
                                                "Minimal",
                                                "Low",
                                                "High",
                                                "XHigh",
                                                "Max",
                                                "Medium"
                                            ]
                                        }
                                    }

                                    model_picker := View {
                                        width: 226
                                        height: 28
                                        visible: false
                                        flow: Down
                                        clip_x: false
                                        clip_y: false

                                        model_drop := ModelDropDown {
                                            labels: [
                                                "antigravity/gemini-3.6-flash",
                                                "antigravity/gemini-3.5-flash",
                                                "antigravity/gemini-3.1-pro",
                                                "antigravity/claude-sonnet-4-6",
                                                "antigravity/claude-opus-4-6",
                                                "antigravity/gpt-oss-120b",
                                                "gpt-5.4",
                                                "gpt-5.4-mini",
                                                "gpt-5.5",
                                                "gpt-5.6-sol",
                                                "gpt-5.6-terra",
                                                "gpt-5.3-codex-spark",
                                                "gpt-4o",
                                                "gpt-4o-mini",
                                                "gpt-5.6-luna"
                                            ]
                                        }

                                    }

                                    context_window := mod.components.ContextWindow {
                                        width: 28
                                        height: 28
                                        visible: false
                                    }

                                    composer_action_slot := View {
                                        width: 34
                                        height: 30
                                        flow: Right

                                        send_btn := mod.components.ComposerAction {
                                            width: 34
                                            height: 30
                                            margin: 0
                                            padding: 0
                                            spacing: 0
                                            text: ""
                                            align: Align{x: 0.5 y: 0.5}
                                            icon_walk: Walk{width: 15 height: 15}
                                            draw_icon +: {
                                                svg: crate_resource("self:resources/icons/send.svg")
                                                color: theme.color_primary_foreground
                                            }
                                        }

                                        stop_btn := mod.components.ComposerAction {
                                            width: 34
                                            height: 30
                                            visible: false
                                            margin: 0
                                            padding: 0
                                            spacing: 0
                                            text: ""
                                            align: Align{x: 0.5 y: 0.5}
                                            icon_walk: Walk{width: 12 height: 12}
                                            draw_bg +: {
                                                color: theme.color_destructive
                                                color_hover: theme.color_destructive
                                                color_focus: theme.color_destructive
                                                color_down: theme.color_destructive
                                                border_color: theme.color_transparent
                                                border_color_hover: theme.color_transparent
                                                border_color_focus: theme.color_transparent
                                                border_color_down: theme.color_transparent
                                                border_size: 0.0
                                            }
                                            draw_icon +: {
                                                svg: crate_resource("self:resources/icons/stop.svg")
                                                color: theme.color_primary_foreground
                                                color_hover: theme.color_primary_foreground
                                                color_focus: theme.color_primary_foreground
                                                color_down: theme.color_primary_foreground
                                            }
                                        }
                                    }
                                }
                            }
                            checkout_target_row := View {
                                width: Fill
                                height: Fit
                                flow: Down
                                spacing: 4
                                padding: Inset{left: 9 right: 7 bottom: 5}

                                checkout_target_controls := RoundedView {
                                    width: Fill
                                    height: 32
                                    flow: Right
                                    spacing: 6
                                    padding: Inset{left: 8 right: 8}
                                    align: Align{y: 0.5}
                                    draw_bg +: {
                                        color: theme.color_card
                                        border_color: theme.color_border
                                        border_size: 1.0
                                        border_radius: theme.radius_sm
                                    }

                                    checkout_target_drop := mod.components.IconDropDown {
                                        width: 142
                                        height: 28
                                        labels: ["New worktree…", "Current checkout"]
                                        use_provider_icons: false
                                        padding: Inset{left: 8 right: 22}
                                        icon_walk: Walk{width: 11 height: 11 margin: Inset{right: 5}}
                                        draw_icon +: {
                                            svg: crate_resource("self:resources/icons/folder.svg")
                                            color: theme.color_primary
                                        }
                                        popup_menu: mod.components.IconPopupMenu {
                                            width: 142
                                            menu_item: mod.components.IconPopupMenuItem {
                                                use_provider_icons: false
                                            }
                                        }
                                    }

                                    flex_spacer := View {
                                        width: Fill
                                        height: 28
                                    }

                                    git_branch_drop := mod.components.GitBranchDropDown {
                                        width: 132
                                        height: 28
                                        labels: ["Git"]
                                    }
                                }

                                worktree_prompt_row := View {
                                    width: Fill
                                    height: 28
                                    visible: false
                                    flow: Right
                                    spacing: 4

                                    worktree_name := mod.components.SearchInput {
                                        width: 110
                                        empty_text: "Worktree name"
                                        margin: 0
                                    }
                                    worktree_path := mod.components.SearchInput {
                                        width: Fill
                                        empty_text: "Path"
                                        margin: 0
                                    }
                                    worktree_create_btn := mod.components.HeaderChipButton {
                                        width: Fit
                                        height: 28
                                        text: "Create"
                                        padding: Inset{left: 7 right: 7 top: 4 bottom: 4}
                                    }
                                    worktree_cancel_btn := mod.components.HeaderChipButton {
                                        width: Fit
                                        height: 28
                                        text: "Cancel"
                                        padding: Inset{left: 7 right: 7 top: 4 bottom: 4}
                                    }
                                }
                            }
                            project_terminal := mod.components.ProjectTerminal {}
                            }

                            right_sidebar_resize_handle := View {
                                width: 6
                                height: Fill
                                visible: false
                                show_bg: true
                                draw_bg +: {
                                    color: theme.color_card
                                    pixel: fn() {
                                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                        sdf.clear(theme.color_transparent)
                                        sdf.rect(self.rect_size.x * 0.5 - 0.5, 0.0, 1.0, self.rect_size.y)
                                        return sdf.fill(self.color)
                                    }
                                }
                            }

                            right_sidebar := View {
                                width: 280
                                height: Fill
                                visible: false
                                flow: Down
                                spacing: 8

                                git_actions := View {
                                    width: Fill
                                    height: Fill
                                    visible: false
                                    flow: Down
                                    spacing: 7
                                    padding: Inset{left: 10 top: 10 right: 10 bottom: 10}
                                    draw_bg +: {
                                        color: theme.color_input
                                        border_color: theme.color_border
                                        border_size: 1.0
                                        border_radius: 8.0
                                    }
                                git_state_row := View {
                                    width: Fill
                                    height: 18
                                    flow: Right
                                    spacing: 6
                                    align: Align{y: 0.5}
                                    git_state_icon := Icon {
                                        width: 12
                                        height: 12
                                        icon_walk: Walk{width: 10 height: 10}
                                        align: Align{x: 0.5 y: 0.5}
                                        draw_icon +: {
                                            svg: crate_resource("self:resources/icons/git.svg")
                                            color: theme.color_primary
                                        }
                                    }
                                    git_state_label := ClippedLabel {
                                        width: Fill
                                        height: 16
                                        text: "SOURCE CONTROL"
                                        align: Align{y: 0.5}
                                        draw_text +: {
                                            color: theme.color_muted_foreground
                                            text_style: theme.font_code { font_size: 8.0 }
                                        }
                                    }
                                }
                                    git_feedback_error_row := View {
                                        width: Fill
                                        height: 18
                                        visible: false
                                        flow: Right
                                        spacing: 5
                                        align: Align{y: 0.5}
                                        git_feedback_error_dot := mod.components.StatusDot {
                                            visible: true
                                            draw_bg +: { color: theme.color_destructive }
                                        }
                                        git_feedback_error := ClippedLabel {
                                            width: Fill
                                            height: 16
                                            align: Align{y: 0.5}
                                            draw_text +: {
                                                color: theme.color_destructive
                                                text_style: theme.font_regular { font_size: 8.0 }
                                            }
                                        }
                                    }
                                    git_feedback_success_row := View {
                                        width: Fill
                                        height: 18
                                        visible: false
                                        flow: Right
                                        spacing: 5
                                        align: Align{y: 0.5}
                                        git_feedback_success_dot := mod.components.StatusDot {
                                            visible: true
                                            draw_bg +: { color: theme.color_success }
                                        }
                                        git_feedback_success := ClippedLabel {
                                            width: Fill
                                            height: 16
                                            align: Align{y: 0.5}
                                            draw_text +: {
                                                color: theme.color_success
                                                text_style: theme.font_regular { font_size: 8.0 }
                                            }
                                        }
                                    }
                                git_changes_header := View {
                                    width: Fill
                                    height: 24
                                    flow: Right
                                    align: Align{y: 0.5}
                                    git_changes_title := Label {
                                        width: Fill
                                        height: 16
                                        align: Align{y: 0.5}
                                        text: "WORKING TREE"
                                        draw_text +: {
                                            color: theme.color_foreground
                                            text_style: theme.font_bold { font_size: 8.0 }
                                        }
                                    }
                                    git_selection_label := ClippedLabel {
                                        width: Fit
                                        height: 16
                                        align: Align{y: 0.5}
                                        draw_text +: {
                                            color: theme.color_muted_foreground
                                            text_style: theme.font_code { font_size: 7.5 }
                                        }
                                    }
                                    git_refresh_btn := mod.components.IconButton {
                                        width: 24
                                        height: 24
                                        text: ""
                                        icon_walk: Walk{width: 12 height: 12}
                                        align: Align{x: 0.5 y: 0.5}
                                        draw_icon +: {
                                            svg: crate_resource("self:resources/icons/refresh.svg")
                                            color: theme.color_muted_foreground
                                            color_hover: theme.color_foreground
                                            color_down: theme.color_primary_foreground
                                        }
                                    }
                                    git_select_all_btn := mod.components.IconButton {
                                        width: 24
                                        height: 24
                                        text: ""
                                        icon_walk: Walk{width: 12 height: 12}
                                        align: Align{x: 0.5 y: 0.5}
                                        draw_icon +: {
                                            svg: crate_resource("self:resources/icons/select-all.svg")
                                            color: theme.color_muted_foreground
                                            color_hover: theme.color_foreground
                                            color_down: theme.color_primary_foreground
                                        }
                                    }
                                }
                                git_changes_wrap := View {
                                    width: Fill
                                    height: Fill
                                    git_changes := mod.components.GitChanges {
                                        width: Fill
                                        height: Fill
                                    }
                                }
                                git_diff_wrap := View {
                                    width: Fill
                                    height: Fill
                                    visible: false
                                    flow: Down
                                    spacing: 5
                                    git_diff_header := View {
                                        width: Fill
                                        height: 24
                                        flow: Right
                                        spacing: 6
                                        align: Align{y: 0.5}
                                        git_diff_back_btn := mod.components.IconButton {
                                            width: 20
                                            height: 20
                                            text: ""
                                            icon_walk: Walk{width: 11 height: 11}
                                            align: Align{x: 0.5 y: 0.5}
                                            padding: 0
                                            spacing: 0
                                            draw_icon +: {
                                                svg: crate_resource("self:resources/icons/back.svg")
                                                color: theme.color_muted_foreground
                                                color_hover: theme.color_foreground
                                                color_down: theme.color_primary_foreground
                                            }
                                        }
                                        git_diff_path := ClippedLabel {
                                            width: Fill
                                            height: 20
                                            align: Align{y: 0.5}
                                            draw_text +: {
                                                color: theme.color_foreground
                                                text_style: theme.font_code { font_size: 8.5 }
                                            }
                                        }
                                        git_diff_loading := View {
                                            width: Fit
                                            height: 18
                                            visible: false
                                            align: Align{y: 0.5}
                                            loading_label := ClippedLabel {
                                                width: Fit
                                                height: 16
                                                text: "Loading…"
                                                align: Align{y: 0.5}
                                                draw_text +: {
                                                    color: theme.color_muted_foreground
                                                    text_style: theme.font_regular { font_size: 8.0 }
                                                }
                                            }
                                        }
                                    }
                                    git_diff_text := mod.components.GitDiffView {
                                        width: Fill
                                        height: Fill
                                    }
                                }
                                git_commit_section := View {
                                    width: Fill
                                    height: Fit
                                    flow: Down
                                    spacing: 5
                                    git_commit_header := View {
                                        width: Fill
                                        height: 24
                                        flow: Right
                                        align: Align{y: 0.5}
                                        git_commit_label := ClippedLabel {
                                            width: Fill
                                            height: 16
                                            text: "COMMIT MESSAGE"
                                            align: Align{y: 0.5}
                                            draw_text +: {
                                                color: theme.color_muted_foreground
                                                text_style: theme.font_bold { font_size: 7.5 }
                                            }
                                        }
                                        git_generate_commit_btn := mod.components.IconButton {
                                            width: 24
                                            height: 24
                                            text: ""
                                            icon_walk: Walk{width: 12 height: 12}
                                            align: Align{x: 0.5 y: 0.5}
                                            draw_icon +: {
                                                svg: crate_resource("self:resources/icons/sparkles.svg")
                                                color: theme.color_primary
                                                color_hover: theme.color_foreground
                                                color_down: theme.color_primary_foreground
                                            }
                                        }
                                    }
                                    git_commit_message := TextInput {
                                        width: Fill
                                        height: Fit{min: FitBound.Abs(64), max: FitBound.Abs(140)}
                                        is_multiline: true
                                        empty_text: "Commit message (required)"
                                        padding: Inset{left: 10 right: 10 top: 8 bottom: 8}
                                        draw_bg +: {
                                            color: theme.color_card
                                            color_hover: theme.color_card
                                            color_focus: theme.color_background
                                            color_down: theme.color_background
                                            border_color: theme.color_border
                                            border_color_hover: theme.color_border
                                            border_color_focus: theme.color_primary
                                            border_color_down: theme.color_primary
                                            border_radius: 8.0
                                            border_size: 1.0
                                        }
                                        draw_text +: {
                                            color: theme.color_foreground
                                            color_hover: theme.color_foreground
                                            color_focus: theme.color_foreground
                                            color_empty: theme.color_muted_foreground
                                            color_empty_hover: theme.color_muted_foreground
                                            color_empty_focus: theme.color_muted_foreground
                                            text_style +: {
                                                font_size: 9.0
                                                line_spacing: 1.35
                                            }
                                        }
                                    }
                                    git_action_row := View {
                                        width: Fill
                                        height: 28
                                        flow: Right
                                        spacing: 4
                                        git_commit_btn := mod.components.HeaderChipButton {
                                            width: Fill
                                            height: 28
                                            text: "Commit"
                                            padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
                                            draw_bg +: {
                                                color: theme.color_primary
                                                color_hover: theme.color_primary
                                                color_focus: theme.color_primary
                                                color_down: theme.color_primary
                                                border_color: theme.color_primary
                                                border_color_hover: theme.color_primary
                                                border_color_focus: theme.color_primary
                                                border_color_down: theme.color_primary
                                            }
                                            draw_text +: {
                                                color: theme.color_primary_foreground
                                                color_hover: theme.color_primary_foreground
                                                color_focus: theme.color_primary_foreground
                                                color_down: theme.color_primary_foreground
                                            }
                                        }
                                        git_push_btn := mod.components.HeaderChipButton {
                                            width: Fill
                                            height: 28
                                            text: "Push"
                                            padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
                                        }
                                        git_pull_btn := mod.components.HeaderChipButton {
                                            width: Fill
                                            height: 28
                                            text: "Pull"
                                            padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
                                        }
                                    }
                                    git_pr_row := View {
                                        width: Fill
                                        height: 28
                                        flow: Right
                                        spacing: 4
                                        git_pr_btn := mod.components.HeaderChipButton {
                                            width: Fill
                                            height: 28
                                            text: "Create Pull Request"
                                            padding: Inset{left: 6 right: 6 top: 4 bottom: 4}
                                            draw_bg +: {
                                                color: theme.color_card
                                                color_hover: theme.color_primary
                                                color_focus: theme.color_primary
                                                color_down: theme.color_primary
                                                border_color: theme.color_border
                                                border_color_hover: theme.color_primary
                                                border_color_focus: theme.color_primary
                                                border_color_down: theme.color_primary
                                            }
                                            draw_text +: {
                                                color: theme.color_foreground
                                                color_hover: theme.color_primary_foreground
                                                color_focus: theme.color_primary_foreground
                                                color_down: theme.color_primary_foreground
                                            }
                                        }
                                    }
                                }
                                }
                                task_sidebar_wrap := View {
                                    width: Fill
                                    height: Fill
                                    visible: false
                                    task_sidebar := mod.components.TaskSidebar {
                                        width: Fill
                                        height: Fill
                                    }
                                }
                                file_tree_wrap := View {
                                    width: Fill
                                    height: Fill
                                    visible: false
                                    flow: Down
                                    padding: Inset{left: 10 top: 10 right: 10 bottom: 10}
                                    draw_bg +: {
                                        color: theme.color_input
                                        border_color: theme.color_border
                                        border_size: 1.0
                                        border_radius: 8.0
                                    }
                                    file_tree := mod.components.FileTree {
                                        width: Fill
                                        height: Fill
                                    }
                                }
                                code_editor_wrap := View {
                                    width: Fill
                                    height: Fill
                                    visible: false
                                    flow: Down
                                    spacing: 6
                                    padding: Inset{left: 10 top: 10 right: 10 bottom: 10}
                                    draw_bg +: {
                                        color: theme.color_input
                                        border_color: theme.color_border
                                        border_size: 1.0
                                        border_radius: 8.0
                                    }

                                    code_editor_header := View {
                                        width: Fill
                                        height: 24
                                        flow: Right
                                        spacing: 6
                                        align: Align{y: 0.5}

                                        code_editor_path_lbl := mod.components.ClippedLabel {
                                            width: Fill
                                            height: 16
                                            text: ""
                                            align: Align{y: 0.5}
                                            draw_text +: {
                                                color: theme.color_muted_foreground
                                                text_style: theme.font_code { font_size: 8.0 }
                                            }
                                        }
                                        code_editor_save_btn := mod.components.IconButton {
                                            width: 26
                                            height: 22
                                            text: ""
                                            icon_walk: Walk{width: 13 height: 13}
                                            align: Align{x: 0.5 y: 0.5}
                                            padding: 0
                                            spacing: 0
                                            draw_icon +: {
                                                svg: crate_resource("self:resources/icons/write-file.svg")
                                                color: theme.color_muted_foreground
                                                color_hover: theme.color_success
                                                color_down: theme.color_primary
                                            }
                                        }
                                        code_editor_close_btn := mod.components.IconButton {
                                            width: 26
                                            height: 22
                                            text: ""
                                            icon_walk: Walk{width: 13 height: 13}
                                            align: Align{x: 0.5 y: 0.5}
                                            padding: 0
                                            spacing: 0
                                            draw_icon +: {
                                                svg: crate_resource("self:resources/icons/close.svg")
                                                color: theme.color_muted_foreground
                                                color_hover: theme.color_destructive
                                                color_down: theme.color_primary
                                            }
                                        }
                                    }

                                    code_editor_status_lbl := Label {
                                        width: Fill
                                        height: Fit
                                        padding: 0
                                        text: ""
                                        draw_text +: {
                                            color: theme.color_destructive
                                            text_style +: { font_size: 9.0 }
                                        }
                                    }

                                    code_editor_view := mod.components.CodeEditorView {}
                                }
                                right_sidebar_tabs := View {
                                    width: Fill
                                    height: 36
                                    flow: Right
                                    align: Align{x: 0.5 y: 0.5}
                                    spacing: 8
                                    padding: Inset{left: 8 right: 8 top: 4 bottom: 4}
                                    draw_bg +: {
                                        color: theme.color_card
                                        border_color: theme.color_border
                                        border_size: 1.0
                                        border_radius: 6.0
                                    }
                                    tasks_tab_btn := mod.components.RightSidebarTabButton {
                                        draw_icon +: { svg: crate_resource("self:resources/icons/subagent.svg") }
                                    }
                                    git_tab_btn := mod.components.RightSidebarTabButton {
                                        draw_icon +: { svg: crate_resource("self:resources/icons/git.svg") }
                                    }
                                    code_editor_tab_btn := mod.components.RightSidebarTabButton {
                                        draw_icon +: { svg: crate_resource("self:resources/icons/edit-file.svg") }
                                    }
                                    file_tree_tab_btn := mod.components.RightSidebarTabButton {
                                        draw_icon +: { svg: crate_resource("self:resources/icons/folder.svg") }
                                    }
                                }
                            }
                        }

                        git_branch_dialog := View {
                            width: Fill
                            height: Fill
                            visible: false
                            flow: Overlay
                            align: Align{x: 0.5 y: 0.5}

                            git_branch_dialog_backdrop := mod.components.ModalDialogBackdrop {}
                            git_branch_dialog_card := RoundedView {
                                width: 380
                                height: Fit
                                flow: Down
                                spacing: 16
                                padding: Inset{left: 20 top: 18 right: 20 bottom: 20}
                                draw_bg +: {
                                    color: theme.color_popover
                                    border_color: theme.color_border
                                    border_size: 1.0
                                    border_radius: theme.radius_lg
                                }

                                git_branch_dialog_title := Label {
                                    width: Fill
                                    height: Fit
                                    text: "Create new branch"
                                    draw_text +: {
                                        color: theme.color_foreground
                                        text_style: theme.font_bold { font_size: 14.0 }
                                    }
                                }
                                git_branch_dialog_name := TextInput {
                                    width: Fill
                                    height: 36
                                    empty_text: "Branch name"
                                    padding: Inset{left: 10 right: 10}
                                    draw_bg +: {
                                        color: theme.color_input
                                        color_focus: theme.color_input
                                        border_color: theme.color_border
                                        border_color_focus: theme.color_primary
                                        border_size: 1.0
                                        border_radius: 7.0
                                    }
                                }
                                git_branch_dialog_actions := View {
                                    width: Fill
                                    height: Fit
                                    flow: Right
                                    spacing: 8
                                    align: Align{x: 1.0 y: 0.5}

                                    git_branch_dialog_cancel_btn := mod.components.HeaderChipButton {
                                        width: Fit
                                        height: 30
                                        text: "Cancel"
                                        padding: Inset{left: 10 right: 10 top: 5 bottom: 5}
                                    }
                                    git_branch_dialog_create_btn := mod.components.HeaderChipButton {
                                        width: Fit
                                        height: 30
                                        text: "Create"
                                        padding: Inset{left: 10 right: 10 top: 5 bottom: 5}
                                    }
                                }
                            }
                        }
                    }

                        }
                    }
                }
            }
        }
    }
}
}

#[derive(Clone, Debug)]
enum ImagePickerAction {
    Loaded {
        key: SessionKey,
        attachment: Result<Option<ImageAttachment>, String>,
    },
}

const MAX_IMAGE_ATTACHMENTS: usize = 4;
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

fn image_attachment_from_rgba(
    display_name: String,
    width: usize,
    height: usize,
    rgba: &[u8],
) -> Result<ImageAttachment, String> {
    if width == 0 || height == 0 {
        return Err("Clipboard image has invalid dimensions".to_string());
    }
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "Clipboard image dimensions are too large".to_string())?;
    if rgba.len() != expected_len {
        return Err("Clipboard image has invalid pixel data".to_string());
    }

    let max_dim = 1600;
    let (target_w, target_h, src_rgba) = if width > max_dim || height > max_dim {
        let scale = (max_dim as f64) / (width.max(height) as f64);
        let tw = ((width as f64 * scale) as usize).max(1);
        let th = ((height as f64 * scale) as usize).max(1);
        let mut resized = vec![0u8; tw * th * 4];
        for y in 0..th {
            let src_y = ((y as f64 / scale) as usize).min(height - 1);
            for x in 0..tw {
                let src_x = ((x as f64 / scale) as usize).min(width - 1);
                let src_idx = (src_y * width + src_x) * 4;
                let dst_idx = (y * tw + x) * 4;
                resized[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
            }
        }
        (tw, th, std::borrow::Cow::Owned(resized))
    } else {
        (width, height, std::borrow::Cow::Borrowed(rgba))
    };

    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, target_w as u32, target_h as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder
            .write_header()
            .map_err(|error| format!("Could not encode clipboard image: {error}"))?;
        writer
            .write_image_data(&src_rgba)
            .map_err(|error| format!("Could not encode clipboard image: {error}"))?;
    }
    if encoded.len() as u64 > MAX_IMAGE_BYTES {
        return Err("Clipboard image is larger than 10 MB after encoding".to_string());
    }

    Ok(ImageAttachment {
        display_name,
        data_url: format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(encoded)
        ),
    })
}

#[cfg(test)]
mod image_attachment_tests {
    use super::*;

    #[test]
    fn rejects_zero_dimensions_before_resizing() {
        assert!(image_attachment_from_rgba("clipboard.png".into(), 0, 1601, &[]).is_err());
    }
}

fn clipboard_image_attachment() -> Result<Option<ImageAttachment>, String> {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(_) => return Ok(None),
    };
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(_) => return Ok(None),
    };
    image_attachment_from_rgba(
        "clipboard.png".to_string(),
        image.width,
        image.height,
        image.bytes.as_ref(),
    )
    .map(Some)
}

fn load_image_attachment(path: &Path) -> Result<ImageAttachment, String> {
    let display_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Could not read {display_name}: {error}"))?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!("{display_name} is larger than 10 MB"));
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("Could not read {display_name}: {error}"))?;
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return Err(format!(
            "{display_name} is not a supported PNG, JPEG, GIF, or WebP image"
        ));
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ImageAttachment {
        display_name,
        data_url: format!("data:{mime};base64,{encoded}"),
    })
}

#[derive(Clone, Copy, PartialEq)]
enum UiStatus {
    Ready,
    Working,
    Error,
}

#[derive(Clone, Copy, PartialEq)]
enum InputOrigin {
    Composer,
    Internal,
}

impl InputOrigin {
    fn consumes_composer(self) -> bool {
        self == Self::Composer
    }
}

fn clear_composer_for_dispatch(origin: InputOrigin, composer: &mut WorkspaceUiState) {
    if origin.consumes_composer() {
        composer.draft.clear();
        composer.attachments.clear();
    }
}

fn extension_reload_matches(
    scope: ExtensionScope,
    changed_project: &Path,
    runtime_project: &Path,
) -> bool {
    scope == ExtensionScope::Global || changed_project == runtime_project
}

struct ExtensionReloadOutcome {
    reloaded: usize,
    failures: Vec<String>,
}

fn session_reload_count(result: Result<usize, String>) -> Result<usize, String> {
    result.map(|_| 1)
}

fn aggregate_extension_reload_results(
    results: impl IntoIterator<Item = (String, Result<usize, String>)>,
) -> ExtensionReloadOutcome {
    let mut outcome = ExtensionReloadOutcome {
        reloaded: 0,
        failures: Vec::new(),
    };
    for (label, result) in results {
        match result {
            Ok(reloaded) => outcome.reloaded += reloaded,
            Err(error) => outcome.failures.push(format!("{label}: {error}")),
        }
    }
    outcome
}

fn extension_reload_status(reloaded: usize, failures: &[String]) -> String {
    if failures.is_empty() {
        return match reloaded {
            0 => "Extension updated on disk; no live sessions were open.".to_owned(),
            1 => "Reloaded extensions in 1 live session.".to_owned(),
            count => format!("Reloaded extensions in {count} live sessions."),
        };
    }

    let failures = truncate_chars(&failures.join("; "), 180);
    match reloaded {
        0 => format!("Live reload failed for {failures}"),
        1 => format!("Reloaded 1 live session; failed for {failures}"),
        count => format!("Reloaded {count} live sessions; failed for {failures}"),
    }
}

fn task_sidebar_items(
    records: Vec<TaskRecord>,
    mut session_label: impl FnMut(&TaskRecord) -> Option<String>,
) -> Vec<TaskSidebarItem> {
    records
        .into_iter()
        .map(|record| {
            let cancellable = record.cancellable();
            let resumable = record.kind == TaskKind::Background && !record.active();
            let label = session_label(&record).unwrap_or_else(|| {
                if record.session_id == "draft" {
                    "Project draft".to_owned()
                } else {
                    record.session_id.clone()
                }
            });
            TaskSidebarItem {
                id: record.id,
                session_id: record.session_id,
                session_label: label,
                session_file: record.session_file,
                agent: record.agent,
                summary: truncate_chars(&record.summary, 120),
                activity: record.current_activity.unwrap_or_default(),
                status: record.status,
                cancellable,
                resumable,
                started_at_ms: record.started_at_ms,
                finished_at_ms: record.finished_at_ms,
            }
        })
        .collect()
}

struct GenerationRun {
    id: u64,
    handle: tokio::task::JoinHandle<()>,
}

/// A live ACP conversation attached to one chat session.
///
/// The update channel is created with the session and outlives a single turn:
/// the handler owns the sender for as long as the session lives, so a follow-up
/// turn must reuse this receiver rather than making a fresh one, which would
/// have no sender and yield nothing.
#[derive(Clone)]
pub struct AcpChat {
    session: Arc<threadlane_coding_agent::AcpSession>,
    updates: Arc<
        tokio::sync::Mutex<
            tokio::sync::mpsc::UnboundedReceiver<threadlane_coding_agent::AcpSessionNotification>,
        >,
    >,
}

struct SessionRuntime {
    agent: Arc<tokio::sync::Mutex<CodingAgent>>,
    cancellation: CodingAgentCancellation,
    /// Live external agent conversation, when this chat is driven over ACP.
    acp: Option<AcpChat>,
    work_handle: CodingAgentWorkHandle,
    session_file: Option<PathBuf>,
    generation: Option<GenerationRun>,
    terminal_generation_id: Option<u64>,
    submitted_draft: Option<(u64, String)>,
    submitted_attachments: Option<(u64, Vec<ImageAttachment>)>,
    status: UiStatus,
    status_text: String,
    model: String,
    reasoning_effort: ReasoningEffort,
    plan: SessionPlan,
    latest_usage: Option<TokenUsage>,
}

#[derive(Clone)]
struct ProjectCapabilities {
    summary: String,
    button_text: String,
    commands: Vec<CommandInfo>,
}

impl SessionRuntime {
    fn new(agent: CodingAgent, model: String, reasoning_effort: ReasoningEffort) -> Self {
        let session_file = agent.session_tree.file_path.clone();
        let plan = agent.current_plan();
        let work_handle = agent.work_handle();
        let cancellation = agent.cancellation_handle();
        Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            cancellation,
            acp: None,
            work_handle,
            session_file,
            generation: None,
            terminal_generation_id: None,
            submitted_draft: None,
            submitted_attachments: None,
            status: UiStatus::Ready,
            status_text: String::new(),
            model,
            reasoning_effort,
            plan,
            latest_usage: None,
        }
    }
}

#[derive(Script)]
pub struct App {
    #[live]
    pub ui: WidgetRef,
    #[rust]
    tx: Option<Sender<GuiAgentEvent>>,
    #[rust]
    rx: Option<Arc<Mutex<Receiver<GuiAgentEvent>>>>,
    #[rust]
    session_runtimes: HashMap<SessionKey, SessionRuntime>,
    #[rust]
    next_generation_id: u64,
    #[rust]
    next_extension_reload_id: u64,
    #[rust]
    busy: bool,
    #[rust]
    composer_state: ComposerState,
    #[rust]
    pending_queue_text: Option<String>,
    #[rust]
    pending_queue_attachments: Vec<ImageAttachment>,
    #[rust]
    commands: Vec<CommandInfo>,
    #[rust]
    capabilities_summary: String,
    #[rust]
    available_models: Vec<String>,
    #[rust]
    workspace_state: AppState,
    #[rust]
    session_context_entry: Option<SessionEntry>,
    #[rust]
    sidebar_pointer: Option<Vec2d>,
    #[rust]
    left_sidebar_open: bool,
    #[rust]
    auth_workspace: Option<SessionKey>,
    #[rust]
    project_registry: Option<ProjectRegistry>,
    #[rust]
    capability_cache: HashMap<PathBuf, ProjectCapabilities>,
    #[rust]
    capability_state: CapabilityState,
    #[rust]
    supervisor: Option<Arc<HarnessSupervisor>>,
    #[rust]
    supervisor_projects: HashMap<PathBuf, String>,
    #[rust]
    task_sidebar_open: bool,
    #[rust]
    update_status: UpdateStatus,
    #[rust]
    update_rx: Option<Arc<Mutex<Receiver<UpdateStatus>>>>,
    #[rust]
    starter_prompt_focus_pending: bool,
    #[rust]
    project_terminals: HashMap<PathBuf, ProjectTerminalGroup>,
    #[rust]
    terminal_poll_next_frame: NextFrame,
    #[rust]
    chat_redraw_next_frame: NextFrame,
    #[rust]
    chat_redraw_pending: bool,
    #[rust]
    next_git_request_id: u64,
    #[rust]
    git_status_timer: Timer,
    #[rust]
    git_status_pending: bool,
    #[rust]
    git_status: HashMap<PathBuf, GitStatus>,
    #[rust]
    checkout_targets: HashMap<SessionKey, PathBuf>,
    #[rust]
    worktree_prompt_open: bool,
    #[rust]
    pending_worktree_path: Option<PathBuf>,
    #[rust]
    git_new_branch_open: bool,
    #[rust]
    git_diff_open: bool,
    #[rust]
    git_diff_pending: bool,
    #[rust]
    git_diff_request_id: u64,
    #[rust]
    git_operation_pending: bool,
    #[rust]
    git_operation_request_id: u64,
    #[rust]
    git_pr_pending: bool,

    #[rust]
    git_pr_created: bool,
    #[rust]
    git_commit_message_pending: bool,
    #[rust]
    git_commit_message_request_id: u64,
    #[rust]
    git_commit_message_abort: Option<tokio::task::AbortHandle>,
    #[rust]
    right_sidebar_tab: RightSidebarTab,
    #[rust]
    code_editor_status: Option<String>,
    #[rust]
    right_sidebar_open: bool,
    #[rust]
    right_sidebar_agents_available: bool,
    #[rust]
    right_sidebar_width: f64,
    #[rust]
    right_sidebar_resizing: bool,
    #[rust]
    right_sidebar_resize_start_x: f64,
    #[rust]
    right_sidebar_resize_start_width: f64,
    #[rust]
    git_feedback: Option<(bool, String)>,
}

impl ScriptHook for App {}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        self.terminal_poll_next_frame = NextFrame::default();
        self.chat_redraw_next_frame = NextFrame::default();
        self.chat_redraw_pending = false;
        self.git_status_timer = cx.start_interval(2.0);
        self.right_sidebar_width = 280.0;
        self.left_sidebar_open = true;
        self.right_sidebar_open = true;
        let (tx, rx) = channel::<GuiAgentEvent>();
        self.tx = Some(tx);
        self.rx = Some(Arc::new(Mutex::new(rx)));
        self.set_model_dropup_options(
            cx,
            include_connected_provider_models(vec![
                "gpt-5.6-luna".into(),
                "gpt-5.4".into(),
                "gpt-5.4-mini".into(),
                "gpt-5.5".into(),
                "gpt-5.6-sol".into(),
                "gpt-5.6-terra".into(),
                "gpt-5.3-codex-spark".into(),
                "gpt-4o".into(),
                "gpt-4o-mini".into(),
            ]),
            default_model_name(),
        );
        self.set_reasoning_effort_picker(cx, ReasoningEffort::Medium);

        let launch_dir = resolve_initial_launch_dir();
        let mut registry_error = None;
        match ProjectRegistry::load(&global_threadlane_dir()) {
            Ok(mut registry) => {
                if registry.projects().is_empty() {
                    if let Err(error) = registry.attach(&launch_dir) {
                        registry_error = Some(error.to_string());
                    }
                }
                self.project_registry = Some(registry);
            }
            Err(error) => registry_error = Some(error.to_string()),
        }
        let supervisor = Arc::new(HarnessSupervisor::new(
            global_threadlane_dir().join("supervisor"),
        ));
        if let Some(registry) = &self.project_registry {
            for project in registry.projects() {
                if let Ok(record) = supervisor.register_project(&project.path) {
                    let path = std::fs::canonicalize(&project.path)
                        .unwrap_or_else(|_| project.path.clone());
                    self.supervisor_projects.insert(path, record.id);
                }
            }
        }
        if let Some(tx) = self.tx.clone() {
            let mut events = supervisor.subscribe();
            get_runtime().spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            let _ = tx.send(GuiAgentEvent::BackgroundTask(event));
                            SignalToUI::set_ui_signal();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
        self.supervisor = Some(supervisor);
        let project_dirs = self.registered_project_dirs_or(&launch_dir);
        refresh_sessions(&project_dirs);
        let selected_project = self
            .project_registry
            .as_ref()
            .and_then(|registry| {
                registry
                    .projects()
                    .iter()
                    .filter(|project| project.path.is_dir())
                    .max_by_key(|project| project.last_opened_at)
            })
            .cloned();
        let work_dir = selected_project
            .as_ref()
            .map(|project| project.path.clone())
            .unwrap_or_else(|| launch_dir.clone());
        let initial_entry = {
            let data = crate::panels::sessions::state::SESSIONS_DATA
                .read()
                .unwrap();
            data.projects
                .iter()
                .find(|project| project.work_dir == work_dir)
                .and_then(|project| {
                    selected_project
                        .as_ref()
                        .and_then(|selected| selected.last_session_id.as_deref())
                        .and_then(|session_id| {
                            project
                                .sessions
                                .iter()
                                .find(|session| session.id == session_id)
                        })
                        .or_else(|| project.sessions.first())
                })
                .cloned()
        };
        if let Some(entry) = initial_entry.as_ref() {
            set_active_session(&entry.work_dir, &entry.id);
            self.select_workspace(entry.work_dir.clone(), entry.id.clone());
        } else {
            set_active_project(&work_dir);
            self.workspace_state
                .select(SessionKey::project_draft(work_dir.clone()));
        }

        let mut key_opt = None;
        let mut account_id_opt = None;

        if let Some(creds) = auth::load_credentials() {
            self.ui
                .text_input(cx, ids!(api_key_input))
                .set_text(cx, &creds.access_token);
            self.push_chat(
                MsgRole::System,
                format!("Loaded saved credentials from {}", creds.source),
            );
            key_opt = Some(creds.access_token.clone());
            account_id_opt = creds.account_id.clone();
        }
        if has_connected_provider() {
            self.ui.widget(cx, ids!(auth_row)).set_visible(cx, false);
            self.set_status(cx, UiStatus::Ready, "Ready");
        } else {
            self.ui.widget(cx, ids!(auth_row)).set_visible(cx, true);
            self.set_status(cx, UiStatus::Error, "Not signed in");
        }

        let home_dir = std::env::var_os("HOME").map(PathBuf::from);
        self.ui
            .label(cx, ids!(project_name_label))
            .set_text(cx, &project_name(&work_dir));
        self.ui
            .label(cx, ids!(workspace_label))
            .set_text(cx, &compact_workspace_path(&work_dir, home_dir.as_deref()));
        let context = ProjectContext::discover(&work_dir);
        if let Some(error) = registry_error {
            self.push_chat(
                MsgRole::System,
                format!("Could not load the attached-project registry: {error}"),
            );
        }

        if !context.context_files.is_empty() {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Discovered {} context file(s): {:?}",
                    context.context_files.len(),
                    context.context_files
                ),
            );
        }

        let api_key = key_opt
            .clone()
            .unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default());
        let agent_opts = CodingAgentOptions {
            api_key: api_key.clone(),
            account_id: account_id_opt.clone(),
            model: "gpt-5.6-luna".to_string(),
            work_dir: work_dir.clone(),
            session_file: initial_entry
                .as_ref()
                .map(|entry| entry.session_file.clone()),
            system_prompt: Default::default(),
        };

        let coding_agent = CodingAgent::new(agent_opts);
        let initial_model = coding_agent
            .session_tree
            .model
            .clone()
            .unwrap_or_else(|| "gpt-5.6-luna".to_string());
        let discovered_skills: Vec<_> = coding_agent
            .skills
            .list_skills()
            .into_iter()
            .filter(|skill| skill.enabled && skill.is_valid)
            .collect();
        let discovered_agents = discover_agents(&work_dir, AgentScope::Both).agents;
        self.capabilities_summary =
            format_capabilities_summary(&discovered_skills, &discovered_agents);
        self.ui.button(cx, ids!(caps_btn)).set_text(
            cx,
            &format_capabilities_button_text(discovered_skills.len(), discovered_agents.len()),
        );

        self.commands = builtin_commands();
        self.commands
            .extend(discovered_skills.iter().map(|skill| CommandInfo {
                name: format!("skill {}", skill.id),
                description: format!(
                    "{} · {}",
                    skill.scope.display_name(),
                    truncate_chars(&normalize_catalog_text(&skill.description), 120)
                ),
            }));
        for manifest in coding_agent.wasi_extensions.extension_manifests() {
            let mut cmd_names = Vec::new();
            for cmd in &manifest.commands {
                cmd_names.push(format!("/{}", cmd.name));
                self.commands.push(CommandInfo {
                    name: cmd.name.clone(),
                    description: cmd.description.clone(),
                });
            }
            self.push_chat(
                MsgRole::System,
                format!(
                    "Loaded WASI extension `{}` ({}) — commands: {}",
                    manifest.name,
                    manifest.description,
                    cmd_names.join(", ")
                ),
            );
        }
        self.push_chat(
            MsgRole::System,
            "Type / in the input bar to browse slash commands.",
        );

        let initial_key = self
            .workspace_state
            .active_key()
            .cloned()
            .unwrap_or_else(|| SessionKey::project_draft(work_dir));
        let latest_usage = coding_agent
            .session_tree
            .get_fact(CONTEXT_USAGE_FACT)
            .and_then(|value| serde_json::from_str::<TokenUsage>(value).ok());
        if let Some(entry) = initial_entry {
            let messages = coding_agent.session_tree.get_active_branch_messages();
            let session_file = entry.session_file.clone();
            let workspace = self
                .workspace_state
                .workspace_mut(SessionKey::new(entry.work_dir.clone(), entry.id.clone()));
            workspace.chat.replace_from_agent_messages(&messages);
            workspace.chat.harness_activities = restore_harness_activities(&session_file);
            set_session_health(
                &entry.work_dir,
                &entry.id,
                session_health(&workspace.chat.harness_activities),
            );
        }
        let mut runtime =
            SessionRuntime::new(coding_agent, initial_model.clone(), ReasoningEffort::Medium);
        runtime.latest_usage = latest_usage;
        self.session_runtimes.insert(initial_key, runtime);
        self.set_model_dropup_options(cx, self.available_models.clone(), &initial_model);

        self.spawn_model_fetch(api_key, account_id_opt);
        self.trigger_update_check(cx);
        self.sync_terminal_project(cx);
        self.request_git_status();
        self.sync_task_sidebar(cx);
        self.sync_context_window(cx);

        cx.redraw_all();
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if let Some(action) =
            actions.find_widget_action(self.ui.text_input(cx, ids!(sidebar_search)).widget_uid())
        {
            if let TextInputAction::Changed(query) = action.cast() {
                set_search_query(query);
                self.ui.widget(cx, ids!(session_list)).redraw(cx);
            }
        }
        let terminal_actions = self
            .ui
            .project_terminal(cx, ids!(project_terminal))
            .actions(actions);
        for action in terminal_actions {
            match action {
                crate::components::terminal_panel::ProjectTerminalAction::Input(bytes) => {
                    self.write_terminal_bytes(cx, bytes);
                }
                crate::components::terminal_panel::ProjectTerminalAction::Key {
                    key,
                    shift,
                    control,
                    alt,
                } => self.write_terminal_key(cx, key, shift, control, alt),
                crate::components::terminal_panel::ProjectTerminalAction::LayoutChanged {
                    cols,
                    rows,
                } => {
                    if self
                        .active_terminal_project()
                        .and_then(|work_dir| self.project_terminals.get(&work_dir))
                        .is_none_or(|group| group.sessions.is_empty())
                    {
                        self.create_project_terminal(cx);
                    }
                    self.resize_project_terminals(cx, cols, rows);
                }
                crate::components::terminal_panel::ProjectTerminalAction::New => {
                    self.create_project_terminal(cx);
                }
                crate::components::terminal_panel::ProjectTerminalAction::Select(index) => {
                    self.select_project_terminal(cx, index);
                }
                crate::components::terminal_panel::ProjectTerminalAction::Close(index) => {
                    self.close_project_terminal(cx, index);
                }
                crate::components::terminal_panel::ProjectTerminalAction::None => {}
            }
        }
        for action in actions {
            if let Some(ImagePickerAction::Loaded { key, attachment }) =
                action.downcast_ref::<ImagePickerAction>()
            {
                self.apply_image_picker_result(cx, key.clone(), attachment.clone());
            }
        }

        if self
            .ui
            .button(cx, ids!(terminal_header_btn))
            .clicked(actions)
        {
            self.ui
                .project_terminal(cx, ids!(project_terminal))
                .toggle(cx);
        }

        if self
            .ui
            .button(cx, ids!(left_sidebar_toggle_btn))
            .clicked(actions)
            || self
                .ui
                .button(cx, ids!(left_sidebar_expand_btn))
                .clicked(actions)
        {
            self.toggle_left_sidebar(cx);
        }

        if self.ui.button(cx, ids!(settings_btn)).clicked(actions) {
            self.open_providers_modal(cx);
            self.refresh_provider_connection_ui(cx);
        }

        if self
            .ui
            .button(cx, ids!(right_sidebar_toggle_btn))
            .clicked(actions)
        {
            self.right_sidebar_open = !self.right_sidebar_open;
            self.sync_right_sidebar(cx);
        }

        if self.ui.button(cx, ids!(close_modal_btn)).clicked(actions) {
            self.dismiss_providers_modal(cx);
        }

        if self
            .ui
            .button(cx, ids!(antigravity_login_btn))
            .clicked(actions)
        {
            if threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some() {
                match threadlane_provider::antigravity_auth::clear_antigravity_credentials() {
                    Ok(()) => self.push_chat(MsgRole::System, "Disconnected Google Antigravity."),
                    Err(e) => self.push_chat(
                        MsgRole::System,
                        format!("Failed to disconnect Google Antigravity: {e}"),
                    ),
                }
                self.refresh_provider_connection_ui(cx);
            } else {
                self.dismiss_providers_modal(cx);
                self.start_antigravity_login(cx);
            }
        }

        if self
            .ui
            .button(cx, ids!(antigravity_doctor_btn))
            .clicked(actions)
        {
            self.dismiss_providers_modal(cx);
            self.start_antigravity_doctor(cx);
        }

        if let Some(action) = actions
            .iter()
            .find_map(|action| action.downcast_ref::<StarterPromptAction>().copied())
        {
            self.apply_starter_prompt(cx, action);
        }

        if let Some(action) = actions
            .iter()
            .find_map(|action| action.downcast_ref::<SubagentRailAction>().cloned())
        {
            let Some(key) = self.workspace_state.active_key().cloned() else {
                return;
            };
            let Some(runtime) = self.session_runtimes.get(&key) else {
                return;
            };
            let cancellation = runtime.cancellation.clone();
            let agent = runtime.agent.clone();
            let session_file = runtime.session_file.clone();
            match action {
                SubagentRailAction::Abort(activity_key) => {
                    if let Some(run_id) = activity_key.strip_prefix("main-") {
                        let run_id = run_id.to_owned();
                        let tx = self.tx.clone();
                        let work_dir = key.work_dir.clone();
                        let session_id = key.session_id.clone();
                        get_runtime().spawn(async move {
                            let result = agent
                                .lock()
                                .await
                                .cancel_suspended_deferred(&run_id)
                                .await
                                .map(|_| true);
                            if let Some(tx) = tx {
                                let _ = tx.send(GuiAgentEvent::HarnessResumeFinished {
                                    work_dir,
                                    session_id,
                                    result,
                                });
                                SignalToUI::set_ui_signal();
                            }
                        });
                    } else if let Err(error) = cancellation.cancel() {
                        self.push_chat(MsgRole::System, format!("Harness abort failed: {error}"));
                    }
                    if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                        workspace.chat.harness_activities = restore_harness_activities(
                            session_file.as_deref().unwrap_or(Path::new("")),
                        );
                    }
                    self.ui.widget(cx, ids!(chat_list)).redraw(cx);
                }
                SubagentRailAction::Resume(activity_key) => {
                    let tx = self.tx.clone();
                    let work_dir = key.work_dir.clone();
                    let session_id = key.session_id.clone();
                    get_runtime().spawn(async move {
                        let result = if let Some(run_id) = activity_key.strip_prefix("main-") {
                            let run_id = run_id.to_owned();
                            agent
                                .lock()
                                .await
                                .redeem_suspended_deferred_from_provider(&run_id)
                                .await
                        } else {
                            agent.lock().await.resume_suspended_harness().await
                        };
                        if let Some(tx) = tx {
                            let _ = tx.send(GuiAgentEvent::HarnessResumeFinished {
                                work_dir,
                                session_id,
                                result,
                            });
                            SignalToUI::set_ui_signal();
                        }
                    });
                }
            }
        }

        let openai_login_clicked = self.ui.button(cx, ids!(openai_login_btn)).clicked(actions);
        let openai_creds = auth::load_credentials();
        let openai_own_connected = openai_creds
            .as_ref()
            .is_some_and(|c| auth::is_own_source(&c.source));
        if openai_login_clicked && openai_own_connected {
            match auth::remove_credentials() {
                Ok(()) => self.push_chat(MsgRole::System, "Disconnected ChatGPT."),
                Err(e) => self.push_chat(
                    MsgRole::System,
                    format!("Failed to disconnect ChatGPT: {e}"),
                ),
            }
            self.refresh_provider_connection_ui(cx);
        }

        let opencode_save_clicked = self.ui.button(cx, ids!(opencode_save_btn)).clicked(actions);
        let opencode_clear_clicked = self
            .ui
            .button(cx, ids!(opencode_clear_btn))
            .clicked(actions);
        if opencode_save_clicked {
            let key = self.ui.text_input(cx, ids!(opencode_api_key_input)).text();
            match threadlane_provider::opencode_auth::save_opencode_api_key(&key) {
                Ok(()) => {
                    self.push_chat(MsgRole::System, "Saved OpenCode API key.");
                    let selected_model = self
                        .ui
                        .icon_drop_down(cx, ids!(model_drop))
                        .selected_label();
                    self.set_model_dropup_options(
                        cx,
                        self.available_models.clone(),
                        &selected_model,
                    );
                    self.refresh_provider_connection_ui(cx);
                }
                Err(error) => self
                    .ui
                    .label(cx, ids!(opencode_status_lbl))
                    .set_text(cx, &error),
            }
        } else if opencode_clear_clicked {
            match threadlane_provider::opencode_auth::clear_opencode_api_key() {
                Ok(()) => {
                    self.ui
                        .text_input(cx, ids!(opencode_api_key_input))
                        .set_text(cx, "");
                    self.push_chat(MsgRole::System, "Cleared OpenCode API key.");
                    self.refresh_provider_connection_ui(cx);
                }
                Err(error) => self
                    .ui
                    .label(cx, ids!(opencode_status_lbl))
                    .set_text(cx, &error),
            }
        }

        let start_openai_login = (openai_login_clicked && openai_creds.is_none())
            || self.ui.button(cx, ids!(login_btn)).clicked(actions);
        if start_openai_login {
            self.dismiss_providers_modal(cx);
            self.auth_workspace = self.workspace_state.active_key().cloned();
            self.push_chat(MsgRole::System, "Initiating ChatGPT device code login...");
            self.apply_status_ui(cx, UiStatus::Working, "Connecting to ChatGPT...");
            cx.redraw_all();

            if let Some(tx) = self.tx.clone() {
                get_runtime().spawn(async move {
                    match auth::start_device_login().await {
                        Ok(resp) => {
                            let _ = tx.send(GuiAgentEvent::DeviceCodePrompt {
                                user_code: resp.user_code.clone(),
                                url: resp.verification_uri.clone(),
                            });
                            SignalToUI::set_ui_signal();

                            loop {
                                tokio::time::sleep(tokio::time::Duration::from_secs(
                                    resp.interval.max(3),
                                ))
                                .await;
                                match auth::poll_device_token(&resp.device_auth_id, &resp.user_code)
                                    .await
                                {
                                    Ok(_tokens) => {
                                        let _ = tx.send(GuiAgentEvent::DeviceLoginSuccess);
                                        SignalToUI::set_ui_signal();
                                        break;
                                    }
                                    Err(e)
                                        if e == "authorization_pending"
                                            || e.contains("pending") =>
                                    {
                                        continue
                                    }
                                    Err(e) => {
                                        let _ = tx.send(GuiAgentEvent::DeviceLoginError(e));
                                        SignalToUI::set_ui_signal();
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(GuiAgentEvent::DeviceLoginError(e));
                            SignalToUI::set_ui_signal();
                        }
                    }
                });
            }
        }

        if self.ui.button(cx, ids!(add_project_btn)).clicked(actions) {
            self.open_project_picker();
        }

        if let Some(action) = actions.find_widget_action(
            self.ui
                .text_input(cx, ids!(git_commit_message))
                .widget_uid(),
        ) {
            if let TextInputAction::Changed(_) = action.cast() {
                self.sync_git_commit_button(cx);
            }
        }

        if self
            .ui
            .button(cx, ids!(git_generate_commit_btn))
            .clicked(actions)
        {
            if self.git_commit_message_pending {
                self.cancel_git_commit_message_generation(cx);
            } else {
                self.start_git_commit_message_generation(cx);
            }
        }

        let commit_requested = self.ui.button(cx, ids!(git_commit_btn)).clicked(actions)
            || self
                .ui
                .text_input(cx, ids!(git_commit_message))
                .returned(actions)
                .is_some();
        if commit_requested {
            let message = self.ui.text_input(cx, ids!(git_commit_message)).text();
            if message.trim().is_empty() {
                self.git_feedback = Some((false, "Enter a commit message first.".to_owned()));
                self.sync_right_sidebar(cx);
            } else {
                self.start_git_commit(cx, message);
            }
        }

        if self.ui.button(cx, ids!(git_push_btn)).clicked(actions) {
            self.start_git_push(cx);
        }

        if self.ui.button(cx, ids!(git_pull_btn)).clicked(actions) {
            self.start_git_pull(cx);
        }

        let cancel_branch_requested = self
            .ui
            .button(cx, ids!(git_branch_dialog_cancel_btn))
            .clicked(actions);
        if cancel_branch_requested {
            self.git_new_branch_open = false;
            self.ui
                .text_input(cx, ids!(git_branch_dialog_name))
                .set_text(cx, "");
            self.sync_right_sidebar(cx);
        }

        let create_branch_requested = self
            .ui
            .button(cx, ids!(git_branch_dialog_create_btn))
            .clicked(actions)
            || self
                .ui
                .text_input(cx, ids!(git_branch_dialog_name))
                .returned(actions)
                .is_some();
        if create_branch_requested {
            let name = self.ui.text_input(cx, ids!(git_branch_dialog_name)).text();
            if name.trim().is_empty() {
                self.git_feedback = Some((false, "Enter a branch name first.".to_owned()));
                self.sync_right_sidebar(cx);
            } else {
                self.start_git_create_branch(cx, name);
            }
        }

        if self
            .ui
            .button(cx, ids!(git_select_all_btn))
            .clicked(actions)
        {
            let changes_widget = self.ui.widget(cx, ids!(git_changes));
            if let Some(mut changes) = changes_widget.borrow_mut::<GitChanges>() {
                changes.toggle_all(cx);
            }
            self.sync_git_selection_ui(cx);
        }

        if self.ui.button(cx, ids!(git_refresh_btn)).clicked(actions) {
            self.git_feedback = None;
            self.sync_right_sidebar(cx);
            self.request_git_status();
        }

        if self.ui.button(cx, ids!(git_diff_back_btn)).clicked(actions) {
            self.close_git_diff(cx);
        }

        let git_changes_uid = self.ui.widget(cx, ids!(git_changes)).widget_uid();
        if let Some(action) = actions.find_widget_action(git_changes_uid) {
            match action.cast::<GitChangesAction>() {
                GitChangesAction::Open(path) => {
                    self.start_git_diff(cx, path);
                }
                GitChangesAction::SelectionChanged => self.sync_git_selection_ui(cx),
                GitChangesAction::None => {}
            }
        }

        let file_tree_uid = self.ui.widget(cx, ids!(file_tree)).widget_uid();
        if let Some(action) = actions.find_widget_action(file_tree_uid) {
            match action.cast::<FileTreeAction>() {
                FileTreeAction::FileClicked(path) => {
                    self.open_file_in_editor(cx, &path);
                }
                FileTreeAction::FolderToggled(_) | FileTreeAction::None => {}
            }
        }

        if self.ui.button(cx, ids!(git_pr_btn)).clicked(actions) {
            self.open_github_pull_request_in_browser(cx);
        }

        if self.ui.button(cx, ids!(tasks_tab_btn)).clicked(actions) {
            self.right_sidebar_tab = RightSidebarTab::Tasks;
            self.task_sidebar_open = true;
            self.sync_right_sidebar(cx);
        }

        if self.ui.button(cx, ids!(git_tab_btn)).clicked(actions) {
            self.right_sidebar_tab = RightSidebarTab::Git;
            self.sync_right_sidebar(cx);
        }

        if self.ui.button(cx, ids!(file_tree_tab_btn)).clicked(actions) {
            self.right_sidebar_tab = RightSidebarTab::FileTree;
            self.sync_right_sidebar(cx);
        }

        // The editor reports its own edits; the unsaved marker in the header
        // only updates if the app refreshes it when that fires.
        let code_editor_uid = self.ui.widget(cx, ids!(code_editor_view)).widget_uid();
        if let Some(action) = actions.find_widget_action(code_editor_uid) {
            if matches!(
                action.cast::<CodeEditorViewAction>(),
                CodeEditorViewAction::Modified
            ) && self.right_sidebar_tab == RightSidebarTab::Editor
            {
                self.sync_code_editor_header(cx);
            }
        }

        if self
            .ui
            .button(cx, ids!(code_editor_tab_btn))
            .clicked(actions)
        {
            self.right_sidebar_tab = RightSidebarTab::Editor;
            self.sync_right_sidebar(cx);
        }

        if self
            .ui
            .button(cx, ids!(code_editor_save_btn))
            .clicked(actions)
        {
            self.save_open_editor_file(cx);
        }

        if self
            .ui
            .button(cx, ids!(code_editor_close_btn))
            .clicked(actions)
        {
            self.ui
                .code_editor_view(cx, ids!(code_editor_view))
                .close(cx);
            self.code_editor_status = None;
            self.right_sidebar_tab = RightSidebarTab::FileTree;
            self.sync_right_sidebar(cx);
        }

        if self
            .ui
            .icon_drop_down(cx, ids!(checkout_target_drop))
            .selected(actions)
            .is_some()
        {
            let selected = self
                .ui
                .icon_drop_down(cx, ids!(checkout_target_drop))
                .selected_label();
            if selected == "New worktree…" {
                self.set_worktree_prompt_visible(cx, true);
            } else {
                self.pending_worktree_path = None;
                if let Some(key) = self.workspace_state.active_key().cloned() {
                    self.checkout_targets.remove(&key);
                }
                self.set_worktree_prompt_visible(cx, false);
                self.rebind_active_runtime_to_target(cx);
                self.sync_git_branch_picker(cx);
                self.request_git_status();
            }
        }

        if self
            .ui
            .button(cx, ids!(worktree_cancel_btn))
            .clicked(actions)
        {
            self.pending_worktree_path = None;
            self.set_worktree_prompt_visible(cx, false);
            self.sync_git_branch_picker(cx);
        }

        if self
            .ui
            .button(cx, ids!(worktree_create_btn))
            .clicked(actions)
            || self
                .ui
                .text_input(cx, ids!(worktree_path))
                .returned(actions)
                .is_some()
        {
            self.start_create_worktree(cx);
        }

        if self
            .ui
            .icon_drop_down(cx, ids!(git_branch_drop))
            .selected(actions)
            .is_some()
        {
            let branch = self
                .ui
                .icon_drop_down(cx, ids!(git_branch_drop))
                .selected_label();
            if self.git_operation_pending || self.git_pr_pending {
                self.sync_git_branch_picker(cx);
            } else if branch == "New branch…" || branch == "＋ New branch…" {
                self.git_new_branch_open = true;
                self.sync_right_sidebar(cx);
                self.ui
                    .view(cx, ids!(git_branch_dialog))
                    .set_visible(cx, true);
                self.ui
                    .text_input(cx, ids!(git_branch_dialog_name))
                    .set_text(cx, "");
                self.ui
                    .text_input(cx, ids!(git_branch_dialog_name))
                    .set_key_focus(cx);
            } else if branch == "Git" || branch == "detached HEAD" {
                self.git_new_branch_open = false;
                self.sync_right_sidebar(cx);
            } else {
                self.git_new_branch_open = false;
                self.checkout_git_branch(cx, branch);
            }
        }

        if self.ui.button(cx, ids!(caps_btn)).clicked(actions) {
            self.open_capabilities_modal(cx);
        }

        self.handle_provider_settings_action(cx, actions);

        let task_sidebar_uid = self.ui.widget(cx, ids!(task_sidebar)).widget_uid();
        if let Some(action) = actions.find_widget_action(task_sidebar_uid) {
            match action.cast::<TaskSidebarAction>() {
                TaskSidebarAction::Close => {
                    self.task_sidebar_open = false;
                    self.sync_task_sidebar(cx);
                }
                TaskSidebarAction::OpenSession {
                    session_id,
                    session_file,
                } => {
                    let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
                        return;
                    };
                    self.refresh_registered_sessions();
                    if let Some(entry) = session_file
                        .as_deref()
                        .and_then(|path| session_entry_for_file(&work_dir, path))
                    {
                        self.activate_session(cx, entry);
                    } else if session_id == "draft" {
                        self.select_project_draft(cx, work_dir);
                    } else {
                        self.push_chat(MsgRole::System, "That task session is not available yet.");
                        self.ui.widget(cx, ids!(chat_list)).redraw(cx);
                    }
                }
                TaskSidebarAction::Cancel(task_id) => {
                    if let Some(supervisor) = &self.supervisor {
                        if let Err(error) = supervisor.cancel_task(&task_id) {
                            self.push_chat(MsgRole::System, error);
                        }
                    }
                    self.sync_task_sidebar(cx);
                }
                TaskSidebarAction::Resume(task_id) => {
                    if let Some(supervisor) = &self.supervisor {
                        match supervisor.resume_task(&task_id) {
                            Ok(()) => {
                                self.push_chat(
                                    MsgRole::System,
                                    format!("Resumed task {task_id}."),
                                );
                            }
                            Err(error) => {
                                self.push_chat(MsgRole::System, error);
                            }
                        }
                    }
                    self.sync_task_sidebar(cx);
                    cx.redraw_all();
                }
                TaskSidebarAction::ToggleSession(session_id) => {
                    if let Some(mut sidebar) = self
                        .ui
                        .widget(cx, ids!(task_sidebar))
                        .borrow_mut::<TaskSidebar>()
                    {
                        sidebar.toggle_session(cx, &session_id);
                    }
                }
                TaskSidebarAction::ToggleTask(_) => {}
                TaskSidebarAction::None => {}
            }
        }

        if self.ui.button(cx, ids!(update_btn)).clicked(actions) {
            match self.update_status.clone() {
                crate::updater::UpdateStatus::Available(info) => {
                    self.trigger_update_download(cx, info);
                }
                crate::updater::UpdateStatus::ReadyToInstall { info, bytes } => {
                    self.trigger_update_install(cx, info, bytes);
                }
                _ => {}
            }
        }

        if self.ui.button(cx, ids!(stop_btn)).clicked(actions) {
            // Dismiss any pending queue popup.
            if self.pending_queue_text.is_some() {
                let text = self.pending_queue_text.take().unwrap_or_default();
                self.pending_queue_attachments.clear();
                self.ui
                    .widget(cx, ids!(queued_message_preview))
                    .set_visible(cx, false);
                // Restore the pending text to the composer.
                self.set_prompt_text(cx, &text);
            }
            self.stop_active_generation(cx);
        }

        let session_menu_uid = self.ui.widget(cx, ids!(session_context_menu)).widget_uid();
        if let Some(action) = actions.find_widget_action(session_menu_uid) {
            match action.cast::<SessionContextMenuAction>() {
                SessionContextMenuAction::Settle => {
                    self.apply_session_context_action(cx, archive_session, "Settled");
                }
                SessionContextMenuAction::Delete => {
                    self.apply_session_context_action(cx, delete_session, "Deleted");
                }
                SessionContextMenuAction::None => {}
            }
        }
        let session_list_uid = self.ui.widget(cx, ids!(session_list)).widget_uid();
        if let Some(action) = actions.find_widget_action(session_list_uid) {
            match action.cast::<SessionListAction>() {
                SessionListAction::ToggleProject(work_dir) => {
                    toggle_project_collapsed(&work_dir);
                    self.ui.widget(cx, ids!(session_list)).redraw(cx);
                }
                SessionListAction::NewSession(work_dir) => {
                    self.create_and_activate_session(cx, work_dir);
                }
                SessionListAction::DetachProject(work_dir) => {
                    self.detach_project(cx, work_dir);
                }
                SessionListAction::None => {}
            }
        }
        let session_list = self.ui.portal_list(cx, ids!(session_list.list));
        for (item_id, item) in session_list.items_with_actions(actions) {
            if let Some(work_dir) = project_work_dir_at_row(item_id) {
                if let Some(action) = actions.find_widget_action(item.widget_uid()) {
                    match action.cast::<ProjectHeaderAction>() {
                        ProjectHeaderAction::Toggle => {
                            toggle_project_collapsed(&work_dir);
                            self.ui.widget(cx, ids!(session_list)).redraw(cx);
                        }
                        ProjectHeaderAction::NewSession => {
                            self.create_and_activate_session(cx, work_dir);
                        }
                        ProjectHeaderAction::Detach => self.detach_project(cx, work_dir),
                        ProjectHeaderAction::None => {}
                    }
                }
                continue;
            }
            if item.button(cx, ids!(overflow_btn)).clicked(actions) {
                if let Some((work_dir, _)) = session_overflow_at_row(item_id) {
                    toggle_project_show_all(&work_dir);
                    self.ui.widget(cx, ids!(session_list)).redraw(cx);
                }
                continue;
            }
            if let Some(action) = actions.find_widget_action(item.widget_uid()) {
                if matches!(action.cast::<SessionRowAction>(), SessionRowAction::Settle) {
                    let Some(entry) = session_entry_at_row(item_id) else {
                        continue;
                    };
                    self.session_context_entry = Some(entry);
                    self.apply_session_context_action(cx, archive_session, "Settled");
                    continue;
                }
            }
            if let Some(fe) = item.as_view().finger_up(actions) {
                if let Some(work_dir) = project_work_dir_at_row(item_id) {
                    if fe.is_over && fe.is_primary_hit() && fe.was_tap() {
                        self.select_project_draft(cx, work_dir);
                    }
                    continue;
                }
                let Some(entry) = session_entry_at_row(item_id) else {
                    continue;
                };
                if fe.is_over
                    && fe.was_tap()
                    && fe
                        .mouse_button()
                        .is_some_and(|button| button.is_secondary())
                {
                    self.session_context_entry = Some(entry.clone());
                    set_session_context_target(Some(&entry));
                    if let Some(mut menu) = self
                        .ui
                        .widget(cx, ids!(session_context_menu))
                        .borrow_mut::<SessionContextMenu>()
                    {
                        menu.open(cx, fe.abs);
                    }
                    cx.redraw_all();
                } else if fe.is_over && fe.is_primary_hit() && fe.was_tap() {
                    if let Some(mut menu) = self
                        .ui
                        .widget(cx, ids!(session_context_menu))
                        .borrow_mut::<SessionContextMenu>()
                    {
                        menu.close(cx);
                    }
                    self.session_context_entry = None;
                    self.activate_session(cx, entry);
                }
            }
        }

        if self.ui.button(cx, ids!(attach_btn)).clicked(actions) && !self.busy {
            self.open_image_picker(cx);
        }
        for (idx, chip_id) in [
            ids!(attachment_chip_0),
            ids!(attachment_chip_1),
            ids!(attachment_chip_2),
            ids!(attachment_chip_3),
        ]
        .into_iter()
        .enumerate()
        {
            if self.ui.button(cx, chip_id).clicked(actions) {
                self.remove_attachment(cx, idx);
            }
        }

        let cti = self
            .ui
            .threadlane_command_text_input(cx, ids!(prompt_input));
        if cti.should_build_items(actions) {
            self.build_cmd_items(cx);
        }
        if let Some(name) = cti.item_selected(actions) {
            let text = format!("/{name} ");
            let text_input = cti.text_input_ref(cx);
            text_input.set_text(cx, &text);
            text_input.set_cursor(
                cx,
                Cursor {
                    index: text.chars().count(),
                    prefer_next_row: false,
                },
                false,
            );
        }

        if self
            .ui
            .icon_drop_down(cx, ids!(effort_drop))
            .selected(actions)
            .is_some()
        {
            let selected_label = self
                .ui
                .icon_drop_down(cx, ids!(effort_drop))
                .selected_label();
            if !self.busy {
                if let Some(effort) = ReasoningEffort::from_label(&selected_label) {
                    if let Some(key) = self.workspace_state.active_key() {
                        if let Some(runtime) = self.session_runtimes.get_mut(key) {
                            runtime.reasoning_effort = effort;
                        }
                    }
                    self.set_reasoning_effort_picker(cx, effort);
                }
            }
        }

        if self
            .ui
            .icon_drop_down(cx, ids!(model_drop))
            .selected(actions)
            .is_some()
        {
            let model_name = self
                .ui
                .icon_drop_down(cx, ids!(model_drop))
                .selected_label();
            if !model_name.is_empty() && !self.busy {
                self.set_model_dropup_options(cx, self.available_models.clone(), &model_name);
                self.dispatch_input(cx, format!("/model {model_name}"), InputOrigin::Internal);
            }
        }

        let submit_prompt = self.ui.button(cx, ids!(send_btn)).clicked(actions)
            || cti.text_input_ref(cx).returned(actions).is_some();
        if submit_prompt {
            let input_text = cti.text_input_ref(cx).text();
            let has_attachments = self
                .workspace_state
                .active_workspace()
                .is_some_and(|workspace| !workspace.ui.attachments.is_empty());
            if !input_text.trim().is_empty() || has_attachments {
                if self.busy {
                    // Show the queue/steer popup instead of immediately steering.
                    let attachments = self
                        .workspace_state
                        .active_workspace()
                        .map(|workspace| workspace.ui.attachments.clone())
                        .unwrap_or_default();
                    self.pending_queue_text = Some(input_text.clone());
                    self.pending_queue_attachments = attachments;
                    self.ui
                        .label(cx, ids!(queued_message_text))
                        .set_text(cx, input_text.trim());
                    self.ui
                        .widget(cx, ids!(queued_message_preview))
                        .set_visible(cx, true);
                    cti.text_input_ref(cx).set_text(cx, "");
                    self.refresh_attachment_ui(cx);
                    cx.redraw_all();
                } else {
                    self.dispatch_input(cx, input_text, InputOrigin::Composer);
                }
            }
        }

        // Queue button: enqueue the pending message as a follow-up.
        if self.ui.button(cx, ids!(queue_btn)).clicked(actions) {
            if let Some(text) = self.pending_queue_text.take() {
                let attachments = std::mem::take(&mut self.pending_queue_attachments);
                self.enqueue_steer_interrupt(cx, &text, attachments);
                self.ui
                    .widget(cx, ids!(queued_message_preview))
                    .set_visible(cx, false);
                cx.redraw_all();
            }
        }

        // Steer button: stop current generation, then dispatch the pending message.
        if self.ui.button(cx, ids!(steer_btn)).clicked(actions) {
            if let Some(text) = self.pending_queue_text.take() {
                let _attachments = std::mem::take(&mut self.pending_queue_attachments);
                self.ui
                    .widget(cx, ids!(queued_message_preview))
                    .set_visible(cx, false);
                // Stop the current generation (same as stop_btn logic).
                self.stop_active_generation(cx);
                // Dispatch the pending message as a fresh prompt.
                self.dispatch_input(cx, text, InputOrigin::Composer);
            }
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::theme_mod(vm);
        crate::theme::install(vm);
        crate::makepad_widgets::widgets_mod(vm);
        crate::components::script_mod(vm);
        self::script_mod(vm)
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // Times the whole pass, including the early return below. No-op unless
        // THREADLANE_PERF=1.
        let _frame = crate::perf::frame();
        if self.handle_clipboard_image_paste(cx, event) {
            return;
        }
        if let Event::MouseDown(mouse_event) = event {
            if mouse_event.button.is_primary() {
                if let Some(action) = self
                    .ui
                    .chat_list(cx, ids!(chat_list))
                    .starter_prompt_at(cx, mouse_event.abs)
                {
                    self.apply_starter_prompt(cx, action);
                }
            }
        }
        self.match_event(cx, event);
        self.poll_agent_events(cx);
        if self.chat_redraw_next_frame.is_event(event).is_some() {
            self.chat_redraw_pending = false;
            self.ui.view(cx, ids!(chat_panel)).redraw(cx);
        }
        self.poll_update_status(cx);
        if self.terminal_poll_next_frame.is_event(event).is_some() {
            self.poll_terminal_output(cx);
            if self.has_live_terminal_sessions() {
                self.terminal_poll_next_frame = cx.new_next_frame();
            }
        }
        if self.git_status_timer.is_event(event).is_some()
            && !self.git_operation_pending
            && !self.git_diff_pending
            && !self.git_pr_pending
            && self
                .active_work_dir()
                .is_some_and(|work_dir| self.git_status.contains_key(work_dir))
        {
            self.request_git_status();
        }
        {
            let mut scope = Scope::with_data(&mut self.workspace_state);
            self.ui.handle_event(cx, event, &mut scope);
        }
        match event {
            Event::MouseDown(pointer)
                if pointer.button.is_primary()
                    && self.right_sidebar_is_visible()
                    && self
                        .ui
                        .view(cx, ids!(right_sidebar_resize_handle))
                        .area()
                        .rect(cx)
                        .contains(pointer.abs) =>
            {
                self.right_sidebar_resizing = true;
                self.right_sidebar_resize_start_x = pointer.abs.x;
                self.right_sidebar_resize_start_width = self.right_sidebar_width;
                cx.set_cursor(MouseCursor::ColResize);
            }
            Event::MouseMove(pointer)
                if self.right_sidebar_resizing && self.right_sidebar_is_visible() =>
            {
                self.set_right_sidebar_width(
                    cx,
                    self.right_sidebar_resize_start_width + self.right_sidebar_resize_start_x
                        - pointer.abs.x,
                );
                cx.set_cursor(MouseCursor::ColResize);
            }
            Event::MouseMove(pointer)
                if self.right_sidebar_is_visible()
                    && self
                        .ui
                        .view(cx, ids!(right_sidebar_resize_handle))
                        .area()
                        .rect(cx)
                        .contains(pointer.abs) =>
            {
                cx.set_cursor(MouseCursor::ColResize);
            }
            Event::MouseUp(pointer) if pointer.button.is_primary() => {
                self.right_sidebar_resizing = false;
            }
            Event::BackPressed { .. } if self.git_diff_open || self.git_diff_pending => {
                self.close_git_diff(cx);
            }
            _ => {}
        }
        if self.starter_prompt_focus_pending
            && (matches!(event, Event::MouseUp(mouse_event) if mouse_event.button.is_primary())
                || matches!(event, Event::Actions(_)))
        {
            self.starter_prompt_focus_pending = false;
            let composer = self
                .ui
                .threadlane_command_text_input(cx, ids!(prompt_input));
            composer.request_text_input_focus();
            composer.redraw(cx);
        }
        self.sync_sidebar_action_visibility(cx, event);
    }
}

const MAX_CAPABILITY_SUMMARY_ITEMS: usize = 32;

fn normalize_catalog_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_capabilities_button_text(skills_count: usize, agents_count: usize) -> String {
    match (skills_count, agents_count) {
        (0, 0) => "Capabilities".to_string(),
        (s, 0) => format!("{s} skills"),
        (0, a) => format!("{a} agents"),
        (s, a) => format!("{s} skills · {a} agents"),
    }
}

fn format_capabilities_summary(skills: &[SkillMetadata], agents: &[AgentConfig]) -> String {
    let mut summary = format!(
        "Capabilities\n\nSkills ({}) — use /skill <id> or let the model load one automatically.",
        skills.len()
    );
    if skills.is_empty() {
        summary.push_str("\n  No skills discovered.");
    } else {
        for skill in skills.iter().take(MAX_CAPABILITY_SUMMARY_ITEMS) {
            summary.push_str(&format!(
                "\n  • {} [{}] — {}",
                truncate_chars(&normalize_catalog_text(&skill.id), 128),
                skill.scope.display_name(),
                truncate_chars(&normalize_catalog_text(&skill.description), 160)
            ));
        }
        if skills.len() > MAX_CAPABILITY_SUMMARY_ITEMS {
            summary.push_str(&format!(
                "\n  … and {} more",
                skills.len() - MAX_CAPABILITY_SUMMARY_ITEMS
            ));
        }
    }

    summary.push_str(&format!(
        "\n\nSubagents ({}) — use /subagent <task>, or let the model delegate automatically.",
        agents.len(),
    ));
    if agents.is_empty() {
        summary.push_str("\n  No agent presets discovered.");
    } else {
        for agent in agents.iter().take(MAX_CAPABILITY_SUMMARY_ITEMS) {
            let model = agent
                .model
                .as_deref()
                .map(|model| format!(" · {}", truncate_chars(&normalize_catalog_text(model), 96)))
                .unwrap_or_default();
            summary.push_str(&format!(
                "\n  • {} [{}{}] — {}",
                truncate_chars(&normalize_catalog_text(&agent.name), 128),
                agent.source.as_str(),
                model,
                truncate_chars(&normalize_catalog_text(&agent.description), 160)
            ));
        }
        if agents.len() > MAX_CAPABILITY_SUMMARY_ITEMS {
            summary.push_str(&format!(
                "\n  … and {} more",
                agents.len() - MAX_CAPABILITY_SUMMARY_ITEMS
            ));
        }
    }
    summary
}

impl App {
    fn sync_left_sidebar(&mut self, cx: &mut Cx) {
        self.ui.dock(cx, ids!(dock)).set_splitter_align(
            cx,
            id!(root),
            left_sidebar_splitter_align(self.left_sidebar_open),
            false,
        );
        self.ui
            .button(cx, ids!(left_sidebar_toggle_btn))
            .set_visible(cx, self.left_sidebar_open);
        self.ui
            .button(cx, ids!(left_sidebar_expand_btn))
            .set_visible(cx, !self.left_sidebar_open);
        crate::components::nav_button::set_selected(
            cx,
            &self.ui.button(cx, ids!(left_sidebar_toggle_btn)),
            self.left_sidebar_open,
        );
        crate::components::nav_button::set_selected(
            cx,
            &self.ui.button(cx, ids!(left_sidebar_expand_btn)),
            self.left_sidebar_open,
        );
        self.ui.view(cx, ids!(header)).redraw(cx);
    }

    fn toggle_left_sidebar(&mut self, cx: &mut Cx) {
        self.left_sidebar_open = !self.left_sidebar_open;
        self.sync_left_sidebar(cx);
    }

    fn stop_active_generation(&mut self, cx: &mut Cx) {
        let active_key = self.workspace_state.active_key().cloned();
        if let Some(key) = active_key {
            if let Some(session_file) = self
                .session_runtimes
                .get(&key)
                .and_then(|runtime| runtime.session_file.as_deref())
            {
                if let Err(error) = cancel_open_subagent_operations(session_file) {
                    self.push_chat(
                        MsgRole::System,
                        format!("Could not persist subagent cancellation: {error}"),
                    );
                }
            }
            let current_draft = self.prompt_text(cx);
            let (restored_draft, restored_attachments, abort_agent) = self
                .session_runtimes
                .get_mut(&key)
                .and_then(|runtime| {
                    let generation = runtime.generation.take()?;
                    let abort_agent = runtime.agent.clone();
                    let _ = runtime.cancellation.cancel();
                    let generation_id = generation.id;
                    generation.handle.abort();
                    // Aborting the task stops us listening, but the external
                    // agent keeps working until it is told to stop.
                    if let Some(chat) = runtime.acp.clone() {
                        get_runtime().spawn(async move {
                            let _ = chat.session.cancel().await;
                        });
                    }
                    runtime.terminal_generation_id = None;
                    let draft = draft_for_cancellation(
                        Some(generation_id),
                        runtime.submitted_draft.as_ref(),
                        generation_id,
                    );
                    let attachments = runtime
                        .submitted_attachments
                        .as_ref()
                        .filter(|(id, _)| *id == generation_id)
                        .map(|(_, att)| att.clone());
                    runtime.submitted_draft = None;
                    runtime.submitted_attachments = None;
                    Some((draft, attachments, Some(abort_agent)))
                })
                .unwrap_or((None, None, None));
            if let Some(agent) = abort_agent {
                let tx = self.tx.clone();
                let work_dir = key.work_dir.clone();
                let session_id = key.session_id.clone();
                get_runtime().spawn(async move {
                    let result = agent
                        .lock()
                        .await
                        .resume_suspended_harness()
                        .await
                        .map(|_| true);
                    if let Some(tx) = tx {
                        let _ = tx.send(GuiAgentEvent::HarnessResumeFinished {
                            work_dir,
                            session_id,
                            result,
                        });
                        SignalToUI::set_ui_signal();
                    }
                });
            }
            let draft = if current_draft.trim().is_empty() {
                restored_draft.unwrap_or_default()
            } else {
                current_draft
            };
            if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                workspace.ui.draft = draft.clone();
                if let Some(attachments) = restored_attachments {
                    workspace.ui.attachments = attachments;
                }
            }
            self.set_prompt_text(cx, &draft);
            self.refresh_attachment_ui(cx);
            self.workspace_state
                .workspace_mut(key.clone())
                .chat
                .mark_generation_stopped();
            if self.finish_session_tasks(&key.work_dir, &key.session_id) {
                self.sync_task_sidebar(cx);
            }
            self.set_session_status(cx, &key, UiStatus::Ready, "Stopped");
            self.push_chat(MsgRole::System, "Generation stopped.");
            self.ui.widget(cx, ids!(chat_list)).redraw(cx);
        }
    }

    fn schedule_chat_redraw(&mut self, cx: &mut Cx) {
        if !self.chat_redraw_pending {
            self.chat_redraw_pending = true;
            self.chat_redraw_next_frame = cx.new_next_frame();
        }
    }
    fn enqueue_steer_interrupt(
        &mut self,
        _cx: &mut Cx,
        input_text: &str,
        attachments: Vec<ImageAttachment>,
    ) {
        let Some(key) = self.workspace_state.active_key().cloned() else {
            return;
        };
        if let Some(runtime) = self.session_runtimes.get(&key) {
            if let Err(error) = runtime
                .work_handle
                .queue_steer_with_images(input_text.to_string(), attachments.clone())
            {
                eprintln!("Failed to persist steer: {error}");
                return;
            }
            let agent = runtime.agent.clone();
            let _ = get_runtime().spawn(async move {
                agent.lock().await.run_scheduled_agent_work().await;
                SignalToUI::set_ui_signal();
            });
        }
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            let attachment_names = attachments
                .iter()
                .map(|attachment| attachment.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let visible = if attachment_names.is_empty() {
                input_text.to_string()
            } else if input_text.trim().is_empty() {
                format!("Attached: {attachment_names}")
            } else {
                format!("{input_text}\n\nAttached: {attachment_names}")
            };
            workspace.chat.push_chat(MsgRole::User, visible);
            workspace.ui.attachments.clear();
        }
    }

    fn open_providers_modal(&mut self, cx: &mut Cx) {
        let mut show_extensions = false;
        let mut show_skills = false;
        let mut show_mcp = false;
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            show_extensions = modal.page == SettingsPage::Capabilities;
            show_skills = modal.page == SettingsPage::Skills;
            show_mcp = modal.page == SettingsPage::McpServers;
            modal.open(cx);
        }
        if show_extensions {
            self.refresh_capability_state(cx);
        }
        if show_skills {
            self.refresh_skill_state(cx);
        }
        if show_mcp {
            self.refresh_mcp_state(cx);
        }
    }

    fn open_capabilities_modal(&mut self, cx: &mut Cx) {
        self.refresh_skill_state(cx);
        self.refresh_capability_state(cx);
        self.refresh_mcp_state(cx);
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            modal.open_page(cx, SettingsPage::Skills);
        }
    }

    fn dismiss_providers_modal(&mut self, cx: &mut Cx) {
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            modal.close(cx);
        }
    }

    fn refresh_capability_state(&mut self, cx: &mut Cx) {
        self.capability_state
            .refresh(&CapabilityCatalog::discover(self.active_work_dir()));
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            modal.set_extension_rows(cx, self.capability_state.extensions.clone());
            modal.set_extension_status(cx, "");
        }
        self.capability_cache.clear();
    }

    fn refresh_skill_state(&mut self, cx: &mut Cx) {
        let work_dir = self.active_work_dir().map(Path::to_path_buf);
        self.capability_state.refresh_skills(work_dir.as_deref());
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            modal.set_skill_rows(cx, self.capability_state.skills.clone());
            modal.set_skill_status(cx, "");
        }
    }

    fn refresh_mcp_state(&mut self, _cx: &mut Cx) {
        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = self.active_work_dir().map(Path::to_path_buf);
        if let Some(tx) = self.tx.clone() {
            get_runtime().spawn(async move {
                let records = threadlane_coding_agent::McpManager::new(global_dir, work_dir)
                    .discover_and_connect()
                    .await;
                let _ = tx.send(GuiAgentEvent::McpRefreshCompleted(records));
                SignalToUI::set_ui_signal();
            });
        }
    }

    fn refresh_live_session_mcp(&self) {
        let agents: Vec<_> = self
            .session_runtimes
            .values()
            .map(|runtime| runtime.agent.clone())
            .collect();
        get_runtime().spawn(async move {
            for agent in agents {
                agent.lock().await.refresh_mcp().await;
            }
        });
    }

    fn set_mcp_enabled(&mut self, cx: &mut Cx, row: usize, enabled: bool) {
        let Some(selected) = self.capability_state.mcp_servers.get(row).cloned() else {
            self.refresh_mcp_state(cx);
            return;
        };
        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = self.active_work_dir().map(Path::to_path_buf);
        let mut configs = match selected.scope {
            threadlane_coding_agent::McpScope::Global => {
                threadlane_coding_agent::McpSettings::load_global(global_dir.as_deref())
            }
            threadlane_coding_agent::McpScope::Project => {
                threadlane_coding_agent::McpSettings::load_project(work_dir.as_deref())
            }
        };
        if let Some(cfg) = configs.iter_mut().find(|c| c.id == selected.id) {
            cfg.enabled = enabled;
            let _ = match selected.scope {
                threadlane_coding_agent::McpScope::Global => {
                    threadlane_coding_agent::McpSettings::save_global(
                        global_dir.as_deref().unwrap(),
                        &configs,
                    )
                }
                threadlane_coding_agent::McpScope::Project => {
                    threadlane_coding_agent::McpSettings::save_project(
                        work_dir.as_deref().unwrap(),
                        &configs,
                    )
                }
            };
        }
        self.refresh_live_session_mcp();
        self.refresh_mcp_state(cx);
    }

    fn remove_mcp_server(&mut self, cx: &mut Cx, row: usize) {
        let Some(selected) = self.capability_state.mcp_servers.get(row).cloned() else {
            self.refresh_mcp_state(cx);
            return;
        };
        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = self.active_work_dir().map(Path::to_path_buf);
        let mut configs = match selected.scope {
            threadlane_coding_agent::McpScope::Global => {
                threadlane_coding_agent::McpSettings::load_global(global_dir.as_deref())
            }
            threadlane_coding_agent::McpScope::Project => {
                threadlane_coding_agent::McpSettings::load_project(work_dir.as_deref())
            }
        };
        configs.retain(|c| c.id != selected.id);
        let _ = match selected.scope {
            threadlane_coding_agent::McpScope::Global => {
                threadlane_coding_agent::McpSettings::save_global(
                    global_dir.as_deref().unwrap(),
                    &configs,
                )
            }
            threadlane_coding_agent::McpScope::Project => {
                threadlane_coding_agent::McpSettings::save_project(
                    work_dir.as_deref().unwrap(),
                    &configs,
                )
            }
        };
        self.refresh_live_session_mcp();
        self.refresh_mcp_state(cx);
    }

    fn add_mcp_server(
        &mut self,
        cx: &mut Cx,
        scope: threadlane_coding_agent::McpScope,
        name: String,
        command: String,
    ) {
        let name = name.trim().to_string();
        let command = command.trim().to_string();
        if name.is_empty() || command.is_empty() {
            if let Some(mut modal) = self
                .ui
                .widget(cx, ids!(providers_modal))
                .borrow_mut::<ProviderSettingsModal>()
            {
                modal.set_mcp_status(
                    cx,
                    "Please provide both a server name and a command or URL.",
                );
            }
            return;
        }
        if command.starts_with("http://") || command.starts_with("https://") {
            if let Some(mut modal) = self
                .ui
                .widget(cx, ids!(providers_modal))
                .borrow_mut::<ProviderSettingsModal>()
            {
                modal.set_mcp_status(
                    cx,
                    "HTTP/SSE MCP servers are not supported yet; use a stdio command.",
                );
            }
            return;
        }

        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = self.active_work_dir().map(Path::to_path_buf);

        if scope == threadlane_coding_agent::McpScope::Project && work_dir.is_none() {
            if let Some(mut modal) = self
                .ui
                .widget(cx, ids!(providers_modal))
                .borrow_mut::<ProviderSettingsModal>()
            {
                modal.set_mcp_status(cx, "Attach a project to add project-scoped MCP servers.");
            }
            return;
        };

        let mut configs = match scope {
            threadlane_coding_agent::McpScope::Global => {
                threadlane_coding_agent::McpSettings::load_global(global_dir.as_deref())
            }
            threadlane_coding_agent::McpScope::Project => {
                threadlane_coding_agent::McpSettings::load_project(work_dir.as_deref())
            }
        };

        let id = name.to_lowercase().replace(' ', "_");

        let mut parts = command.split_whitespace();
        let cmd = parts.next().unwrap_or("").to_string();
        let args: Vec<String> = parts.map(String::from).collect();
        let transport = threadlane_coding_agent::McpTransport::Stdio {
            command: cmd,
            args,
            env: std::collections::HashMap::new(),
        };

        let new_config = threadlane_coding_agent::McpServerConfig {
            id,
            name: name.clone(),
            transport,
            enabled: true,
            scope,
        };

        configs.retain(|c| c.id != new_config.id);
        configs.push(new_config);

        let save_result = match scope {
            threadlane_coding_agent::McpScope::Global => {
                threadlane_coding_agent::McpSettings::save_global(
                    global_dir.as_deref().unwrap(),
                    &configs,
                )
            }
            threadlane_coding_agent::McpScope::Project => {
                threadlane_coding_agent::McpSettings::save_project(
                    work_dir.as_deref().unwrap(),
                    &configs,
                )
            }
        };
        match save_result {
            Ok(()) => {
                self.refresh_live_session_mcp();
                self.refresh_mcp_state(cx);
                if let Some(mut modal) = self
                    .ui
                    .widget(cx, ids!(providers_modal))
                    .borrow_mut::<ProviderSettingsModal>()
                {
                    modal.set_mcp_status(cx, &format!("Added MCP server '{}'.", name));
                }
            }
            Err(e) => {
                if let Some(mut modal) = self
                    .ui
                    .widget(cx, ids!(providers_modal))
                    .borrow_mut::<ProviderSettingsModal>()
                {
                    modal.set_mcp_status(cx, &format!("Failed to save MCP server: {e}"));
                }
            }
        }
    }

    fn set_acp_status(&mut self, cx: &mut Cx, status: &str) {
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            modal.set_acp_status(cx, status);
        }
    }

    fn load_acp_configs(
        scope: threadlane_coding_agent::AcpScope,
        global_dir: Option<&Path>,
        work_dir: Option<&Path>,
    ) -> Vec<threadlane_coding_agent::AcpAgentConfig> {
        match scope {
            threadlane_coding_agent::AcpScope::Global => {
                threadlane_coding_agent::AcpSettings::load_global(global_dir)
            }
            threadlane_coding_agent::AcpScope::Project => {
                threadlane_coding_agent::AcpSettings::load_project(work_dir)
            }
        }
    }

    fn save_acp_configs(
        scope: threadlane_coding_agent::AcpScope,
        global_dir: Option<&Path>,
        work_dir: Option<&Path>,
        configs: &[threadlane_coding_agent::AcpAgentConfig],
    ) -> Result<(), String> {
        match scope {
            threadlane_coding_agent::AcpScope::Global => {
                let dir = global_dir
                    .ok_or_else(|| "No global Threadlane directory is available.".to_string())?;
                threadlane_coding_agent::AcpSettings::save_global(dir, configs)
            }
            threadlane_coding_agent::AcpScope::Project => {
                let root = work_dir.ok_or_else(|| {
                    "Attach a project to manage project-scoped ACP agents.".to_string()
                })?;
                threadlane_coding_agent::AcpSettings::save_project(root, configs)
            }
        }
    }

    /// Probing an agent spawns its process, so refreshes run off the UI thread
    /// and report back through `AcpRefreshCompleted`.
    ///
    /// Configured agents are rendered immediately from disk so the list is not
    /// blank while a slow or missing agent binary is being probed.
    fn refresh_acp_state(&mut self, cx: &mut Cx) {
        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = self.active_work_dir().map(Path::to_path_buf);

        let pending =
            threadlane_coding_agent::AcpManager::new(global_dir.clone(), work_dir.clone())
                .configs()
                .into_iter()
                .map(|config| {
                    let status = if config.enabled {
                        threadlane_coding_agent::AcpAgentStatus::Connecting
                    } else {
                        threadlane_coding_agent::AcpAgentStatus::Disconnected
                    };
                    threadlane_coding_agent::AcpAgentRecord { config, status }
                })
                .collect();
        self.capability_state.refresh_acp_records(pending);
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            modal.set_acp_rows(cx, self.capability_state.acp_agents.clone());
        }

        if let Some(tx) = self.tx.clone() {
            get_runtime().spawn(async move {
                let records = threadlane_coding_agent::AcpManager::new(global_dir, work_dir)
                    .discover_and_connect()
                    .await;
                let _ = tx.send(GuiAgentEvent::AcpRefreshCompleted(records));
                SignalToUI::set_ui_signal();
            });
        }
    }

    fn set_acp_enabled(&mut self, cx: &mut Cx, row: usize, enabled: bool) {
        let Some(selected) = self.capability_state.acp_agents.get(row).cloned() else {
            self.refresh_acp_state(cx);
            self.set_acp_status(cx, "ACP agent list changed. Please try again.");
            return;
        };
        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = self.active_work_dir().map(Path::to_path_buf);
        let mut configs =
            Self::load_acp_configs(selected.scope, global_dir.as_deref(), work_dir.as_deref());
        let Some(config) = configs.iter_mut().find(|c| c.id == selected.id) else {
            self.refresh_acp_state(cx);
            self.set_acp_status(cx, "ACP agent list changed. Please try again.");
            return;
        };
        config.enabled = enabled;
        if let Err(error) = Self::save_acp_configs(
            selected.scope,
            global_dir.as_deref(),
            work_dir.as_deref(),
            &configs,
        ) {
            self.set_acp_status(cx, &format!("Failed to save ACP agent: {error}"));
            return;
        }
        self.set_acp_status(cx, "");
        self.refresh_acp_state(cx);
    }

    fn remove_acp_agent(&mut self, cx: &mut Cx, row: usize) {
        let Some(selected) = self.capability_state.acp_agents.get(row).cloned() else {
            self.refresh_acp_state(cx);
            self.set_acp_status(cx, "ACP agent list changed. Please try again.");
            return;
        };
        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = self.active_work_dir().map(Path::to_path_buf);
        let mut configs =
            Self::load_acp_configs(selected.scope, global_dir.as_deref(), work_dir.as_deref());
        configs.retain(|c| c.id != selected.id);
        if let Err(error) = Self::save_acp_configs(
            selected.scope,
            global_dir.as_deref(),
            work_dir.as_deref(),
            &configs,
        ) {
            self.set_acp_status(cx, &format!("Failed to remove ACP agent: {error}"));
            return;
        }
        self.set_acp_status(cx, &format!("Removed ACP agent '{}'.", selected.name));
        self.refresh_acp_state(cx);
    }

    fn add_acp_agent(
        &mut self,
        cx: &mut Cx,
        scope: threadlane_coding_agent::AcpScope,
        name: String,
        command: String,
    ) {
        let Some(new_config) =
            threadlane_coding_agent::AcpAgentConfig::from_command_line(&name, &command, scope)
        else {
            self.set_acp_status(cx, "Please provide both an agent name and a command.");
            return;
        };
        // ACP has no HTTP transport; an agent is always a local command.
        if command.trim().starts_with("http://") || command.trim().starts_with("https://") {
            self.set_acp_status(
                cx,
                "ACP agents are launched over stdio; provide a command, not a URL.",
            );
            return;
        }

        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = self.active_work_dir().map(Path::to_path_buf);
        if scope == threadlane_coding_agent::AcpScope::Project && work_dir.is_none() {
            self.set_acp_status(cx, "Attach a project to add project-scoped ACP agents.");
            return;
        }

        let mut configs = Self::load_acp_configs(scope, global_dir.as_deref(), work_dir.as_deref());
        configs.retain(|c| c.id != new_config.id);
        let display_name = new_config.name.clone();
        configs.push(new_config);

        match Self::save_acp_configs(scope, global_dir.as_deref(), work_dir.as_deref(), &configs) {
            Ok(()) => {
                self.set_acp_status(cx, &format!("Added ACP agent '{display_name}'."));
                self.refresh_acp_state(cx);
            }
            Err(error) => {
                self.set_acp_status(cx, &format!("Failed to save ACP agent: {error}"));
            }
        }
    }

    fn set_skill_status(&mut self, cx: &mut Cx, status: &str) {
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            modal.set_skill_status(cx, status);
        }
    }

    fn set_skill_enabled(&mut self, cx: &mut Cx, row: usize, enabled: bool) {
        let Some(selected) = self.capability_state.skills.get(row).cloned() else {
            self.refresh_skill_state(cx);
            self.set_skill_status(cx, "Skill inventory changed. Please try again.");
            return;
        };
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            self.set_skill_status(cx, "Attach a project to manage project skills.");
            return;
        };
        let mut settings = SkillSettings::load(&work_dir);
        let status = match settings.set_enabled(&work_dir, &selected.id, enabled) {
            Ok(()) if enabled => format!("Enabled {}.", selected.id),
            Ok(()) => format!("Disabled {}.", selected.id),
            Err(error) => error,
        };
        // Refresh the settings list, the capabilities chip / slash commands, and
        // live session catalogs so the toggle takes effect.
        self.refresh_skill_state(cx);
        self.capability_cache.clear();
        self.refresh_project_capabilities(cx, &work_dir);
        self.refresh_live_session_skills(&work_dir);
        self.set_skill_status(cx, &status);
    }

    /// Rediscover skills for live sessions rooted in the toggled project so their
    /// skill catalog and `load_skill` gating reflect the new enable state.
    fn refresh_live_session_skills(&mut self, work_dir: &Path) {
        let canonical = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        let targets: Vec<_> = self
            .session_runtimes
            .iter()
            .filter(|(key, _)| {
                std::fs::canonicalize(&key.work_dir).unwrap_or_else(|_| key.work_dir.clone())
                    == canonical
            })
            .map(|(_, runtime)| runtime.agent.clone())
            .collect();
        if targets.is_empty() {
            return;
        }
        get_runtime().spawn(async move {
            for agent in targets {
                agent.lock().await.refresh_skills();
            }
        });
    }

    fn open_extension_picker(&self, scope: ExtensionScope) {
        let picked = rfd::FileDialog::new()
            .set_title("Install a compiled WASI extension")
            .add_filter("WebAssembly", &["wasm"])
            .pick_file();
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(GuiAgentEvent::ExtensionFilePicked {
                path: picked,
                scope,
            });
            SignalToUI::set_ui_signal();
        }
    }

    fn extension_manager(&self) -> ExtensionManager {
        ExtensionManager::new(
            default_global_threadlane_dir(),
            self.active_work_dir().map(Path::to_path_buf),
        )
    }

    fn set_capability_status(&mut self, cx: &mut Cx, status: &str) {
        if let Some(mut modal) = self
            .ui
            .widget(cx, ids!(providers_modal))
            .borrow_mut::<ProviderSettingsModal>()
        {
            modal.set_extension_status(cx, status);
        }
    }

    fn reload_extension_runtimes(&mut self, scope: ExtensionScope) {
        self.next_extension_reload_id = self.next_extension_reload_id.wrapping_add(1);
        let reload_id = self.next_extension_reload_id;
        let changed_project = self.active_work_dir().map(Path::to_path_buf);
        let session_targets =
            self.session_runtimes
                .iter()
                .filter(|(key, _)| {
                    changed_project.as_deref().is_some_and(|project| {
                        extension_reload_matches(scope, project, &key.work_dir)
                    }) || scope == ExtensionScope::Global
                })
                .map(|(key, runtime)| (key.clone(), runtime.agent.clone()))
                .collect::<Vec<_>>();
        let supervisor = self.supervisor.clone();
        let tx = self.tx.clone();

        get_runtime().spawn(async move {
            let mut results = Vec::new();
            for (key, agent) in session_targets {
                let result = session_reload_count(agent.lock().await.reload_extensions().await);
                results.push((
                    format!(
                        "session '{}' in '{}'",
                        key.session_id,
                        key.work_dir.display()
                    ),
                    result,
                ));
            }
            if let Some(supervisor) = supervisor {
                results.push((
                    "background tasks".to_owned(),
                    supervisor
                        .reload_extensions(scope, changed_project.as_deref())
                        .await,
                ));
            }

            let outcome = aggregate_extension_reload_results(results);
            if let Some(tx) = tx {
                let _ = tx.send(GuiAgentEvent::ExtensionReloadCompleted {
                    reload_id,
                    reloaded: outcome.reloaded,
                    failures: outcome.failures,
                });
                SignalToUI::set_ui_signal();
            }
        });
    }

    fn install_extension(&mut self, cx: &mut Cx, source: PathBuf, scope: ExtensionScope) {
        let manager = self.extension_manager();
        let (status, reload_scope) = match manager.install_from_wasm(&source, scope) {
            Ok(record) => (
                format!(
                    "Installed {} on disk. Reloading live sessions…",
                    record.name()
                ),
                Some(record.scope()),
            ),
            Err(error) => (error, None),
        };
        if let Some(scope) = reload_scope {
            self.reload_extension_runtimes(scope);
        }
        self.refresh_capability_state(cx);
        self.set_capability_status(cx, &status);
    }

    fn set_extension_enabled(&mut self, cx: &mut Cx, row: usize, enabled: bool) {
        let Some(selected) = self.capability_state.extensions.get(row).cloned() else {
            self.refresh_capability_state(cx);
            self.set_capability_status(cx, "Extension inventory changed. Please try again.");
            return;
        };
        let manager = self.extension_manager();
        let record = manager
            .discover()
            .into_iter()
            .find(|record| selected.matches_record(record));
        let (status, reload_scope) = match record {
            Some(record) => match manager.set_enabled(&record, enabled) {
                Ok(()) if enabled => (
                    format!(
                        "Enabled {} on disk. Reloading live sessions…",
                        record.name()
                    ),
                    Some(record.scope()),
                ),
                Ok(()) => (
                    format!(
                        "Disabled {} on disk. Reloading live sessions…",
                        record.name()
                    ),
                    Some(record.scope()),
                ),
                Err(error) => (error, None),
            },
            None => (
                "Extension inventory changed. Please try again.".to_owned(),
                None,
            ),
        };
        if let Some(scope) = reload_scope {
            self.reload_extension_runtimes(scope);
        }
        self.refresh_capability_state(cx);
        self.set_capability_status(cx, &status);
    }

    fn remove_extension(&mut self, cx: &mut Cx, row: usize) {
        let Some(selected) = self.capability_state.extensions.get(row).cloned() else {
            self.refresh_capability_state(cx);
            self.set_capability_status(cx, "Extension inventory changed. Please try again.");
            return;
        };
        let manager = self.extension_manager();
        let record = manager
            .discover()
            .into_iter()
            .find(|record| selected.matches_record(record));
        let (status, reload_scope) = match record {
            Some(record) => match manager.remove(&record) {
                Ok(()) => (
                    format!(
                        "Removed {} from disk. Reloading live sessions…",
                        record.name()
                    ),
                    Some(record.scope()),
                ),
                Err(error) => (error, None),
            },
            None => (
                "Extension inventory changed. Please try again.".to_owned(),
                None,
            ),
        };
        if let Some(scope) = reload_scope {
            self.reload_extension_runtimes(scope);
        }
        self.refresh_capability_state(cx);
        self.set_capability_status(cx, &status);
    }

    fn refresh_provider_connection_ui(&mut self, cx: &mut Cx) {
        if let Some(creds) = threadlane_provider::antigravity_auth::load_antigravity_credentials() {
            let status_text = match creds.account_email {
                Some(ref email) => format!("✓ Connected ({email})"),
                None => "✓ Connected".to_string(),
            };
            self.ui
                .label(cx, ids!(antigravity_status_lbl))
                .set_text(cx, &status_text);
            self.ui
                .button(cx, ids!(antigravity_login_btn))
                .set_text(cx, "Disconnect");
        } else {
            self.ui
                .label(cx, ids!(antigravity_status_lbl))
                .set_text(cx, "Not Connected");
            self.ui
                .button(cx, ids!(antigravity_login_btn))
                .set_text(cx, "Sign in with Google");
        }
        match auth::load_credentials() {
            Some(creds) if auth::is_own_source(&creds.source) => {
                self.ui
                    .label(cx, ids!(openai_status_lbl))
                    .set_text(cx, "✓ Connected");
                self.ui
                    .button(cx, ids!(openai_login_btn))
                    .set_text(cx, "Disconnect");
                self.ui
                    .button(cx, ids!(openai_login_btn))
                    .set_enabled(cx, true);
            }
            Some(creds) => {
                self.ui
                    .label(cx, ids!(openai_status_lbl))
                    .set_text(cx, &format!("✓ Connected (via {})", creds.source));
                self.ui
                    .button(cx, ids!(openai_login_btn))
                    .set_text(cx, "Managed Externally");
                self.ui
                    .button(cx, ids!(openai_login_btn))
                    .set_enabled(cx, false);
            }
            None => {
                self.ui
                    .label(cx, ids!(openai_status_lbl))
                    .set_text(cx, "Not Connected");
                self.ui
                    .button(cx, ids!(openai_login_btn))
                    .set_text(cx, "Sign in with ChatGPT");
                self.ui
                    .button(cx, ids!(openai_login_btn))
                    .set_enabled(cx, true);
            }
        }
        if let Some(key) = threadlane_provider::opencode_auth::load_opencode_api_key() {
            self.ui
                .label(cx, ids!(opencode_status_lbl))
                .set_text(cx, "✓ Connected");
            self.ui
                .text_input(cx, ids!(opencode_api_key_input))
                .set_text(cx, &key);
            self.ui
                .button(cx, ids!(opencode_save_btn))
                .set_text(cx, "Update API key");
        } else {
            self.ui
                .label(cx, ids!(opencode_status_lbl))
                .set_text(cx, "Not Connected");
            self.ui
                .button(cx, ids!(opencode_save_btn))
                .set_text(cx, "Save API key");
        }
        if has_connected_provider() {
            self.ui.widget(cx, ids!(auth_row)).set_visible(cx, false);
        } else {
            self.ui.widget(cx, ids!(auth_row)).set_visible(cx, true);
            self.set_status(cx, UiStatus::Error, "Not signed in");
        }
        cx.redraw_all();
    }

    fn start_antigravity_login(&mut self, cx: &mut Cx) {
        self.push_chat(
            MsgRole::System,
            "Initiating Google Antigravity OAuth login...",
        );
        let (verifier, challenge) = threadlane_provider::antigravity_auth::generate_pkce_pair();
        let (state, _) = threadlane_provider::antigravity_auth::generate_pkce_pair();
        let auth_url =
            threadlane_provider::antigravity_auth::build_authorization_url(&challenge, &state);

        self.push_chat(
            MsgRole::System,
            format!("Opening Google sign-in in your browser...\n{}", auth_url),
        );
        let _ = robius_open::Uri::new(&auth_url).open();

        let tx_clone = self.tx.clone();
        get_runtime().spawn(async move {
            match threadlane_provider::antigravity_auth::listen_for_oauth_callback(state).await {
                Ok(code) => {
                    match threadlane_provider::antigravity_auth::exchange_code_for_tokens(
                        &code, &verifier,
                    )
                    .await
                    {
                        Ok(creds) => {
                            if let Some(ref tx) = tx_clone {
                                let _ = tx.send(GuiAgentEvent::AntigravityLoginSuccess {
                                    email: creds.account_email,
                                });
                                SignalToUI::set_ui_signal();
                            }
                        }
                        Err(e) => {
                            if let Some(ref tx) = tx_clone {
                                let _ = tx.send(GuiAgentEvent::AntigravityLoginError(e));
                                SignalToUI::set_ui_signal();
                            }
                        }
                    }
                }
                Err(e) => {
                    if let Some(ref tx) = tx_clone {
                        let _ = tx.send(GuiAgentEvent::AntigravityLoginError(e));
                        SignalToUI::set_ui_signal();
                    }
                }
            }
        });
        cx.redraw_all();
    }

    fn start_antigravity_doctor(&mut self, cx: &mut Cx) {
        self.push_chat(MsgRole::System, "Running Antigravity Doctor diagnostics...");
        let tx_clone = self.tx.clone();
        get_runtime().spawn(async move {
            let client = threadlane_provider::antigravity::AntigravityClient::new();
            let report = client.run_diagnostics().await;
            if let Some(ref tx) = tx_clone {
                let _ = tx.send(GuiAgentEvent::AntigravityDoctorReport(report));
                SignalToUI::set_ui_signal();
            }
        });
        cx.redraw_all();
    }

    /// Runs one chat turn against an external ACP agent.
    ///
    /// The session is created on first use and kept on the runtime, so a
    /// follow-up turn continues the same conversation. Updates are translated
    /// to `AgentEvent` and forwarded exactly like a built-in generation.
    #[allow(clippy::too_many_arguments)]
    fn start_acp_generation(
        &mut self,
        cx: &mut Cx,
        key: SessionKey,
        generation_id: u64,
        agent_id: String,
        input: String,
        consumes_composer: bool,
        submitted_draft: String,
        attachments: Vec<ImageAttachment>,
        origin: InputOrigin,
    ) {
        let Some(tx) = self.tx.clone() else {
            return;
        };
        if !attachments.is_empty() {
            // Routed to this turn's session: another workspace may be active
            // by the time a dispatch lands.
            self.push_chat_to(
                key.clone(),
                MsgRole::System,
                "Attachments are not sent to ACP agents yet; only the text was sent.".to_string(),
            );
        }

        let existing = self
            .session_runtimes
            .get(&key)
            .and_then(|runtime| runtime.acp.clone());
        let global_dir = threadlane_coding_agent::default_global_threadlane_dir();
        let work_dir = key.work_dir.clone();
        let event_work_dir = key.work_dir.clone();
        let event_session_id = key.session_id.clone();
        let session_tx = tx.clone();

        let handle = get_runtime().spawn(async move {
            let forward = |event: threadlane_agent::AgentEvent| {
                let _ = tx.send(GuiAgentEvent::GenerationAgent {
                    generation_id,
                    work_dir: event_work_dir.clone(),
                    session_id: event_session_id.clone(),
                    event,
                });
                SignalToUI::set_ui_signal();
            };
            let finish = || {
                let _ = tx.send(GuiAgentEvent::GenerationFinished {
                    generation_id,
                    work_dir: event_work_dir.clone(),
                    session_id: event_session_id.clone(),
                });
                SignalToUI::set_ui_signal();
            };

            // The update channel belongs to the session, not the turn: the
            // handler owns the sender for as long as the session lives, so a
            // follow-up turn reuses this receiver instead of making a fresh one
            // that would have no sender.
            let chat = match existing {
                Some(chat) => chat,
                None => {
                    let (update_tx, update_rx) = tokio::sync::mpsc::unbounded_channel();
                    let handler: Arc<dyn threadlane_coding_agent::AcpClientHandler> = Arc::new(
                        threadlane_coding_agent::AcpWorkspaceClient::new(work_dir.clone())
                            .with_permission_policy(
                                threadlane_coding_agent::AcpPermissionPolicy::AllowOnce,
                            )
                            .with_update_sender(update_tx),
                    );
                    let manager = threadlane_coding_agent::AcpManager::new(
                        global_dir,
                        Some(work_dir.clone()),
                    );
                    match manager.start_session(&agent_id, &work_dir, handler).await {
                        Ok(session) => {
                            let chat = AcpChat {
                                session: Arc::new(session),
                                updates: Arc::new(tokio::sync::Mutex::new(update_rx)),
                            };
                            let _ = session_tx.send(GuiAgentEvent::AcpSessionStarted {
                                work_dir: event_work_dir.clone(),
                                session_id: event_session_id.clone(),
                                chat: chat.clone(),
                            });
                            SignalToUI::set_ui_signal();
                            chat
                        }
                        Err(error) => {
                            forward(threadlane_agent::AgentEvent::AgentError {
                                error: format!("Could not start ACP agent: {error}"),
                            });
                            finish();
                            return;
                        }
                    }
                }
            };
            let session = Arc::clone(&chat.session);
            let mut update_rx = chat.updates.lock().await;

            forward(threadlane_agent::AgentEvent::AgentStart);
            forward(threadlane_agent::AgentEvent::MessageStart {
                role: "assistant".to_string(),
            });

            let prompt = session.prompt_text(&input);
            tokio::pin!(prompt);
            let mut listening = true;
            let outcome = loop {
                tokio::select! {
                    result = &mut prompt => break result,
                    update = update_rx.recv(), if listening => match update {
                        Some(notification) => {
                            for event in
                                threadlane_coding_agent::agent_events_for(notification.update)
                            {
                                forward(event);
                            }
                        }
                        // Only reachable once the session's handler is gone.
                        // Disable the branch rather than spinning on it.
                        None => listening = false,
                    }
                }
            };

            // Drain whatever the agent emitted between its last update and the
            // stop reason, or the tail of a turn is lost.
            while let Ok(notification) = update_rx.try_recv() {
                for event in threadlane_coding_agent::agent_events_for(notification.update) {
                    forward(event);
                }
            }

            match outcome {
                Ok(stop) => {
                    if !matches!(stop, threadlane_coding_agent::AcpStopReason::EndTurn) {
                        forward(threadlane_agent::AgentEvent::AgentError {
                            error: format!("Agent stopped: {stop:?}"),
                        });
                    }
                }
                Err(error) => forward(threadlane_agent::AgentEvent::AgentError {
                    error: format!("ACP turn failed: {error}"),
                }),
            }
            forward(threadlane_agent::AgentEvent::AgentEnd {
                usage: threadlane_agent::TokenUsage::default(),
            });
            finish();
        });

        if let Some(runtime) = self.session_runtimes.get_mut(&key) {
            runtime.generation = Some(GenerationRun {
                id: generation_id,
                handle,
            });
            runtime.terminal_generation_id = None;
            if consumes_composer {
                runtime.submitted_draft = Some((generation_id, submitted_draft));
                runtime.submitted_attachments = Some((generation_id, attachments));
            }
        }
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            clear_composer_for_dispatch(origin, &mut workspace.ui);
        }
        let _ = cx;
    }

    fn set_model_dropup_options(
        &mut self,
        cx: &mut Cx,
        mut models: Vec<String>,
        selected_model: &str,
    ) {
        models = include_connected_provider_models(models);
        // Injected here rather than at each call site so every path that
        // repopulates the picker shows configured ACP agents.
        append_acp_models(
            &mut models,
            threadlane_coding_agent::default_global_threadlane_dir().as_deref(),
            self.active_work_dir(),
        );
        let Some((canonical, display)) = ordered_model_options(models, selected_model) else {
            return;
        };

        let selected_item = display.len() - 1;
        self.available_models = canonical;
        let model_drop = self.ui.icon_drop_down(cx, ids!(model_drop));
        model_drop.set_labels(cx, display);
        model_drop.set_selected_item(cx, selected_item);
    }

    fn set_reasoning_effort_picker(&mut self, cx: &mut Cx, effort: ReasoningEffort) {
        let efforts = [
            ReasoningEffort::Off,
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ];
        let mut ordered: Vec<_> = efforts
            .into_iter()
            .filter(|candidate| *candidate != effort)
            .collect();
        ordered.push(effort);

        let labels = ordered
            .iter()
            .map(|effort| effort.label().to_string())
            .collect();
        let effort_drop = self.ui.icon_drop_down(cx, ids!(effort_drop));
        effort_drop.set_labels(cx, labels);
        effort_drop.set_selected_item(cx, ordered.len() - 1);
    }

    fn sync_context_window(&self, cx: &mut Cx) {
        let indicator = self.ui.context_window(cx, ids!(context_window));
        let Some(key) = self.workspace_state.active_key() else {
            indicator.clear_usage(cx);
            return;
        };
        let Some(runtime) = self.session_runtimes.get(key) else {
            indicator.clear_usage(cx);
            return;
        };
        let Some(usage) = &runtime.latest_usage else {
            indicator.clear_usage(cx);
            return;
        };
        indicator.set_usage(
            cx,
            usage.input_tokens,
            usage.total_tokens,
            context_window_limit(&runtime.model),
        );
    }

    fn refresh_attachment_ui(&self, cx: &mut Cx) {
        let names = self
            .workspace_state
            .active_workspace()
            .map(|workspace| {
                workspace
                    .ui
                    .attachments
                    .iter()
                    .map(|attachment| attachment.display_name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.ui
            .widget(cx, ids!(attachment_row))
            .set_visible(cx, !names.is_empty());
        for (index, path) in [
            ids!(attachment_chip_0),
            ids!(attachment_chip_1),
            ids!(attachment_chip_2),
            ids!(attachment_chip_3),
        ]
        .into_iter()
        .enumerate()
        {
            let chip = self.ui.button(cx, path);
            if let Some(name) = names.get(index) {
                chip.set_text(
                    cx,
                    &format!("{} ×", crate::path_utils::truncate_middle_chars(name, 24)),
                );
                chip.set_visible(cx, true);
            } else {
                chip.set_visible(cx, false);
            }
        }
    }

    fn remove_attachment(&mut self, cx: &mut Cx, index: usize) {
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            if index < workspace.ui.attachments.len() {
                workspace.ui.attachments.remove(index);
            }
        }
        self.refresh_attachment_ui(cx);
    }

    fn handle_clipboard_image_paste(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if self.busy
            || !cx.has_key_focus(
                self.ui
                    .threadlane_command_text_input(cx, ids!(prompt_input))
                    .text_input_ref(cx)
                    .area(),
            )
        {
            return false;
        }
        let is_paste = match event {
            Event::TextInput(input) => input.was_paste,
            Event::KeyDown(key) => {
                key.key_code == KeyCode::KeyV && (key.modifiers.logo || key.modifiers.control)
            }
            _ => false,
        };
        if !is_paste {
            return false;
        }

        match clipboard_image_attachment() {
            Ok(Some(attachment)) => {
                if let Some(key) = self.workspace_state.active_key().cloned() {
                    self.apply_image_picker_result(cx, key, Ok(Some(attachment)));
                }
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.push_chat(MsgRole::System, error);
                cx.redraw_all();
                true
            }
        }
    }

    fn apply_image_picker_result(
        &mut self,
        cx: &mut Cx,
        key: SessionKey,
        result: Result<Option<ImageAttachment>, String>,
    ) {
        if self.workspace_state.workspace(&key).is_none() {
            return;
        }
        match result {
            Ok(Some(attachment)) => {
                let workspace = self.workspace_state.workspace_mut(key.clone());
                if workspace.ui.attachments.len() >= MAX_IMAGE_ATTACHMENTS {
                    self.push_chat_to(
                        key.clone(),
                        MsgRole::System,
                        format!("You can attach up to {MAX_IMAGE_ATTACHMENTS} images per prompt"),
                    );
                } else if !workspace
                    .ui
                    .attachments
                    .iter()
                    .any(|existing| existing.data_url == attachment.data_url)
                {
                    workspace.ui.attachments.push(attachment);
                }
            }
            Ok(None) => {}
            Err(error) => self.push_chat_to(key.clone(), MsgRole::System, error),
        }
        if self.workspace_state.is_active(&key) {
            self.refresh_attachment_ui(cx);
        }
    }

    fn open_image_picker(&mut self, cx: &mut Cx) {
        let Some(key) = self.workspace_state.active_key().cloned() else {
            return;
        };
        if self
            .workspace_state
            .workspace(&key)
            .is_some_and(|workspace| workspace.ui.attachments.len() >= MAX_IMAGE_ATTACHMENTS)
        {
            self.push_chat(
                MsgRole::System,
                format!("You can attach up to {MAX_IMAGE_ATTACHMENTS} images per prompt"),
            );
            return;
        }

        let callback_key = key.clone();
        let result = FileDialog::new()
            .set_title("Attach an image")
            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
            .pick_image(move |result| {
                let attachment = match result {
                    Ok(Some(file)) => match file.into_local_file() {
                        Ok(local_file) => load_image_attachment(local_file.path()).map(Some),
                        Err(error) => Err(format!("Could not access selected image: {error}")),
                    },
                    Ok(None) => Ok(None),
                    Err(error) => Err(format!("Image picker failed: {error}")),
                };
                Cx::post_action(ImagePickerAction::Loaded {
                    key: callback_key,
                    attachment,
                });
            });
        if let Err(error) = result {
            self.push_chat_to(
                key,
                MsgRole::System,
                format!("Image picker failed: {error}"),
            );
            cx.redraw_all();
        }
    }

    fn poll_update_status(&mut self, cx: &mut Cx) {
        let mut new_status = None;
        if let Some(rx) = &self.update_rx {
            if let Ok(rx_guard) = rx.lock() {
                while let Ok(status) = rx_guard.try_recv() {
                    new_status = Some(status);
                }
            }
        }
        if let Some(status) = new_status {
            self.update_status = status;
            self.sync_update_button(cx);
        }
    }

    fn sync_update_button(&mut self, cx: &mut Cx) {
        let btn = self.ui.button(cx, ids!(update_btn));
        let action_row = self.ui.widget(cx, ids!(update_action_row));
        let notice = self.ui.widget(cx, ids!(update_notice));
        let loader = self.ui.widget(cx, ids!(update_notice_loader));
        let available_dot = self.ui.widget(cx, ids!(update_notice_available_dot));
        let ready_dot = self.ui.widget(cx, ids!(update_notice_ready_dot));
        let error_dot = self.ui.widget(cx, ids!(update_notice_error_dot));
        let title = self.ui.label(cx, ids!(update_notice_title));
        let detail = self.ui.label(cx, ids!(update_notice_detail));

        let show_action = matches!(
            self.update_status,
            crate::updater::UpdateStatus::Available(_)
                | crate::updater::UpdateStatus::Downloading { .. }
                | crate::updater::UpdateStatus::ReadyToInstall { .. }
                | crate::updater::UpdateStatus::Installing
        );
        action_row.set_visible(cx, show_action);
        btn.set_visible(cx, show_action);
        notice.set_visible(
            cx,
            matches!(
                self.update_status,
                crate::updater::UpdateStatus::Downloading { .. }
                    | crate::updater::UpdateStatus::ReadyToInstall { .. }
                    | crate::updater::UpdateStatus::Installing
            ),
        );
        loader.set_visible(cx, false);
        available_dot.set_visible(cx, false);
        ready_dot.set_visible(cx, false);
        error_dot.set_visible(cx, false);

        match &self.update_status {
            crate::updater::UpdateStatus::Idle => {}
            crate::updater::UpdateStatus::Checking => {
                btn.set_text(cx, "Checking…");
                loader.set_visible(cx, true);
                title.set_text(cx, "Checking for updates");
                detail.set_text(cx, "Looking for the latest signed release");
            }
            crate::updater::UpdateStatus::Available(info) => {
                btn.set_text(cx, &format!("Update Threadlane · v{}", info.version));
                available_dot.set_visible(cx, true);
                title.set_text(cx, &format!("Threadlane v{} is available", info.version));
                let release_detail = if info.notes.trim().is_empty() {
                    format!(
                        "You’re currently on v{}. The update is signed and ready to download.",
                        crate::updater::CURRENT_VERSION
                    )
                } else {
                    truncate_chars(&normalize_catalog_text(&info.notes), 100)
                };
                detail.set_text(cx, &release_detail);
            }
            crate::updater::UpdateStatus::UpToDate => {}
            crate::updater::UpdateStatus::Downloading { version, progress } => {
                let pct = (progress.clamp(0.0, 1.0) * 100.0).round() as u32;
                btn.set_text(cx, &format!("{pct}% downloaded"));
                loader.set_visible(cx, true);
                title.set_text(cx, &format!("Downloading Threadlane v{version}"));
                detail.set_text(
                    cx,
                    &format!("{pct}% complete · Verifying before installation"),
                );
            }
            crate::updater::UpdateStatus::ReadyToInstall { info, .. } => {
                btn.set_text(cx, "Install");
                ready_dot.set_visible(cx, true);
                title.set_text(cx, &format!("Threadlane v{} is ready", info.version));
                detail.set_text(cx, "Verified · Relaunch to finish installation");
            }
            crate::updater::UpdateStatus::Installing => {
                btn.set_text(cx, "Installing…");
                loader.set_visible(cx, true);
                title.set_text(cx, "Installing update");
                detail.set_text(cx, "Threadlane will relaunch automatically");
            }
            crate::updater::UpdateStatus::Error(err) => {
                btn.set_text(cx, "Retry");
                error_dot.set_visible(cx, true);
                title.set_text(cx, "Couldn’t update Threadlane");
                detail.set_text(cx, &truncate_chars(&normalize_catalog_text(err), 100));
                eprintln!("[Threadlane Updater] Error: {err}");
            }
        }
        cx.redraw_all();
    }

    fn trigger_update_check(&mut self, cx: &mut Cx) {
        self.update_status = crate::updater::UpdateStatus::Checking;
        self.sync_update_button(cx);

        let (tx, rx) = std::sync::mpsc::channel();
        self.update_rx = Some(Arc::new(Mutex::new(rx)));

        get_runtime().spawn_blocking(move || {
            let status = match crate::updater::check_for_update() {
                Ok(Some(info)) => crate::updater::UpdateStatus::Available(info),
                Ok(None) => crate::updater::UpdateStatus::UpToDate,
                Err(err) => crate::updater::UpdateStatus::Error(err),
            };
            let _ = tx.send(status);
            SignalToUI::set_ui_signal();
        });
    }

    fn trigger_update_download(&mut self, cx: &mut Cx, info: crate::updater::UpdateReleaseInfo) {
        self.update_status = crate::updater::UpdateStatus::Downloading {
            version: info.version.clone(),
            progress: 0.0,
        };
        self.sync_update_button(cx);

        let (tx, rx) = std::sync::mpsc::channel();
        self.update_rx = Some(Arc::new(Mutex::new(rx)));

        get_runtime().spawn_blocking(move || {
            let progress_tx = tx.clone();
            let download_version = info.version.clone();
            let result = crate::updater::download_update(&info, move |progress| {
                let _ = progress_tx.send(crate::updater::UpdateStatus::Downloading {
                    version: download_version.clone(),
                    progress,
                });
                SignalToUI::set_ui_signal();
            });

            let status = match result {
                Ok(bytes) => crate::updater::UpdateStatus::ReadyToInstall {
                    info,
                    bytes: Arc::new(bytes),
                },
                Err(err) => crate::updater::UpdateStatus::Error(err),
            };
            let _ = tx.send(status);
            SignalToUI::set_ui_signal();
        });
    }

    fn trigger_update_install(
        &mut self,
        cx: &mut Cx,
        info: crate::updater::UpdateReleaseInfo,
        bytes: Arc<Vec<u8>>,
    ) {
        self.update_status = crate::updater::UpdateStatus::Installing;
        self.sync_update_button(cx);

        let (tx, rx) = std::sync::mpsc::channel();
        self.update_rx = Some(Arc::new(Mutex::new(rx)));

        get_runtime().spawn_blocking(move || {
            let bytes = Arc::try_unwrap(bytes).unwrap_or_else(|bytes| (*bytes).clone());
            if let Err(err) = crate::updater::install_and_relaunch(info, bytes) {
                let _ = tx.send(crate::updater::UpdateStatus::Error(err));
                SignalToUI::set_ui_signal();
            }
        });
    }

    fn registered_project_dirs_or(&self, fallback: &Path) -> Vec<PathBuf> {
        let dirs = self.registered_project_dirs();
        if dirs.is_empty() {
            vec![fallback.to_path_buf()]
        } else {
            dirs
        }
    }

    fn registered_project_dirs(&self) -> Vec<PathBuf> {
        self.project_registry
            .as_ref()
            .map(|registry| {
                registry
                    .projects()
                    .iter()
                    .map(|project| project.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn refresh_registered_sessions(&self) {
        refresh_sessions(&self.registered_project_dirs());
    }

    fn prompt_text(&self, cx: &Cx) -> String {
        self.ui
            .threadlane_command_text_input(cx, ids!(prompt_input))
            .text_input_ref(cx)
            .text()
    }

    fn apply_starter_prompt(&mut self, cx: &mut Cx, action: StarterPromptAction) {
        let prompt = match action {
            StarterPromptAction::Explore => "Explore and explain the codebase — walk me through the project structure, key files, and how things connect.",
            StarterPromptAction::Build => "Let’s build a new feature. Describe what you’d like and I’ll help design and implement it.",
            StarterPromptAction::Review => "Review my code and suggest improvements — look for bugs, style issues, and simplification opportunities.",
            StarterPromptAction::Fix => "Help me diagnose and fix an issue. Describe the problem or share an error message to get started.",
            StarterPromptAction::None => return,
        };
        self.set_prompt_text(cx, prompt);
        self.ui
            .threadlane_command_text_input(cx, ids!(prompt_input))
            .text_input_ref(cx)
            .set_cursor(
                cx,
                Cursor {
                    index: prompt.len(),
                    prefer_next_row: false,
                },
                false,
            );
        self.starter_prompt_focus_pending = true;
        cx.redraw_all();
    }

    fn set_prompt_text(&self, cx: &mut Cx, text: &str) {
        self.ui
            .threadlane_command_text_input(cx, ids!(prompt_input))
            .text_input_ref(cx)
            .set_text(cx, text);
    }

    fn active_work_dir(&self) -> Option<&Path> {
        let key = self.workspace_state.active_key()?;
        Some(
            self.checkout_targets
                .get(key)
                .map_or(key.work_dir.as_path(), PathBuf::as_path),
        )
    }

    fn rebind_active_runtime_to_target(&mut self, cx: &mut Cx) {
        let Some(key) = self.workspace_state.active_key().cloned() else {
            return;
        };
        let Some((model, reasoning_effort, session_file)) =
            self.session_runtimes.get(&key).map(|runtime| {
                (
                    runtime.model.clone(),
                    runtime.reasoning_effort,
                    runtime.session_file.clone(),
                )
            })
        else {
            return;
        };
        let (api_key, account_id) = self.current_credentials(cx);
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            return;
        };
        let agent = CodingAgent::new(CodingAgentOptions {
            api_key,
            account_id,
            model: model.clone(),
            work_dir,
            session_file,
            system_prompt: Default::default(),
        });
        self.session_runtimes
            .insert(key, SessionRuntime::new(agent, model, reasoning_effort));
    }

    fn request_git_status(&mut self) {
        if self.git_status_pending {
            return;
        }
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            return;
        };
        self.next_git_request_id = self.next_git_request_id.wrapping_add(1);
        let request_id = self.next_git_request_id;
        let Some(tx) = self.tx.clone() else {
            return;
        };
        self.git_status_pending = true;
        get_runtime().spawn_blocking(move || {
            let result = crate::git::inspect(&work_dir).map_err(|error| error.message);
            let _ = tx.send(GuiAgentEvent::GitStatusLoaded {
                request_id,
                work_dir,
                result,
            });
            SignalToUI::set_ui_signal();
        });
    }

    fn sync_git_branch_picker(&self, cx: &mut Cx) {
        let status = self
            .active_work_dir()
            .and_then(|work_dir| self.git_status.get(work_dir));
        let (labels, selected) = crate::panels::git::view::git_branch_picker_labels(status);
        self.ui
            .icon_drop_down(cx, ids!(git_branch_drop))
            .set_visible(cx, status.is_some());
        let picker = self.ui.icon_drop_down(cx, ids!(git_branch_drop));
        picker.set_labels(cx, labels);
        picker.set_selected_item(cx, selected);
        let target_selected = self
            .workspace_state
            .active_key()
            .and_then(|key| self.checkout_targets.get(key))
            .map_or(1, |_| 0);
        let target_picker = self.ui.icon_drop_down(cx, ids!(checkout_target_drop));
        target_picker.set_selected_item(cx, target_selected);
        target_picker.set_visible(cx, status.is_some());
    }

    fn set_worktree_prompt_visible(&mut self, cx: &mut Cx, visible: bool) {
        self.worktree_prompt_open = visible;
        self.ui
            .view(cx, ids!(worktree_prompt_row))
            .set_visible(cx, visible);
        if visible {
            self.ui.text_input(cx, ids!(worktree_name)).set_text(cx, "");
            self.ui.text_input(cx, ids!(worktree_path)).set_text(cx, "");
            self.ui
                .text_input(cx, ids!(worktree_name))
                .set_key_focus(cx);
        }
    }

    fn start_create_worktree(&mut self, cx: &mut Cx) {
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            return;
        };
        let name = self.ui.text_input(cx, ids!(worktree_name)).text();
        let path_text = self.ui.text_input(cx, ids!(worktree_path)).text();
        let branch = self
            .ui
            .icon_drop_down(cx, ids!(git_branch_drop))
            .selected_label();
        let path = PathBuf::from(path_text.trim());
        if name.trim().is_empty() || path_text.trim().is_empty() {
            self.git_feedback = Some((false, "Enter a worktree name and path.".into()));
            return;
        }
        if branch.is_empty() || branch == "Git" || branch == "detached HEAD" {
            self.git_feedback = Some((false, "Select a branch before creating a worktree.".into()));
            return;
        }
        self.pending_worktree_path = Some(path.clone());
        self.start_git_operation(cx, format!("create worktree `{name}`"), move |_| {
            crate::git::create_worktree(&work_dir, &path, &branch)
        });
    }

    /// Opens a workspace-relative path from the file tree in the code editor.
    fn open_file_in_editor(&mut self, cx: &mut Cx, rel_path: &str) {
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            return;
        };
        // The tree only ever yields paths under the workspace, but resolving
        // through the shared guard keeps that true if the tree ever changes.
        let absolute = match threadlane_tools::validate_path_in_workspace(rel_path, &work_dir) {
            Ok(path) => path,
            Err(error) => {
                self.code_editor_status = Some(error);
                self.right_sidebar_tab = RightSidebarTab::Editor;
                self.sync_right_sidebar(cx);
                return;
            }
        };

        let editor = self.ui.code_editor_view(cx, ids!(code_editor_view));
        self.code_editor_status = match editor.open_file(cx, &absolute) {
            Ok(()) => None,
            Err(error) => Some(error),
        };
        self.right_sidebar_tab = RightSidebarTab::Editor;
        self.sync_right_sidebar(cx);
    }

    fn save_open_editor_file(&mut self, cx: &mut Cx) {
        let editor = self.ui.code_editor_view(cx, ids!(code_editor_view));
        self.code_editor_status = match editor.save() {
            Ok(()) => None,
            Err(error) => Some(error),
        };
        // A save changes the working tree, so the Git panel is now stale.
        self.request_git_status();
        self.sync_right_sidebar(cx);
    }

    fn sync_code_editor_header(&mut self, cx: &mut Cx) {
        let editor = self.ui.code_editor_view(cx, ids!(code_editor_view));
        let work_dir = self.active_work_dir().map(Path::to_path_buf);
        let label = match editor.path() {
            Some(path) => {
                let shown = work_dir
                    .as_deref()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .unwrap_or(path.as_path())
                    .display()
                    .to_string();
                if editor.is_modified() {
                    format!("{shown} •")
                } else {
                    shown
                }
            }
            None => "No file open".to_string(),
        };
        self.ui
            .label(cx, ids!(code_editor_path_lbl))
            .set_text(cx, &label);

        let status = self.code_editor_status.clone().unwrap_or_default();
        self.ui
            .label(cx, ids!(code_editor_status_lbl))
            .set_text(cx, &status);
        self.ui
            .button(cx, ids!(code_editor_save_btn))
            .set_enabled(cx, editor.is_open());
    }

    fn sync_right_sidebar(&mut self, cx: &mut Cx) {
        if let Some(content_width) = self.content_row_width(cx) {
            self.right_sidebar_width = self.right_sidebar_width.clamp(
                RIGHT_SIDEBAR_MIN_WIDTH,
                self.right_sidebar_max_width_for(content_width),
            );
        }
        let status = self
            .active_work_dir()
            .and_then(|work_dir| self.git_status.get(work_dir));
        let _has_git = status.is_some();
        if let Some(status) = status {
            let state_text = crate::panels::git::view::format_git_summary_text(
                status,
                self.git_operation_pending,
                self.git_commit_message_pending,
            );
            self.ui
                .label(cx, ids!(git_state_label))
                .set_text(cx, &state_text);
            let (feedback_error, feedback_success) = match self.git_feedback.as_ref() {
                Some((is_success, message)) => {
                    if *is_success {
                        self.ui
                            .label(cx, ids!(git_feedback_success))
                            .set_text(cx, message);
                    } else {
                        self.ui
                            .label(cx, ids!(git_feedback_error))
                            .set_text(cx, message);
                    }
                    (!*is_success, *is_success)
                }
                None => (false, false),
            };
            self.ui
                .view(cx, ids!(git_feedback_error_row))
                .set_visible(cx, feedback_error);
            self.ui
                .view(cx, ids!(git_feedback_success_row))
                .set_visible(cx, feedback_success);
            self.ui
                .view(cx, ids!(git_branch_dialog))
                .set_visible(cx, self.git_new_branch_open);
            if let Some(mut changes) = self
                .ui
                .widget(cx, ids!(git_changes))
                .borrow_mut::<GitChanges>()
            {
                changes.set_files(cx, status.files.clone());
            }
            self.sync_git_selection_ui(cx);

            let has_remote = status.remote.is_some();
            let show_commit_section = !self.git_diff_open;

            self.ui
                .view(cx, ids!(git_commit_section))
                .set_visible(cx, show_commit_section);

            if show_commit_section {
                let can_generate = status.has_changes
                    && !self.git_operation_pending
                    && !self.git_commit_message_pending;
                self.ui
                    .button(cx, ids!(git_generate_commit_btn))
                    .set_enabled(cx, can_generate);

                self.sync_git_commit_button(cx);

                self.ui
                    .button(cx, ids!(git_commit_btn))
                    .set_visible(cx, true);

                self.ui
                    .button(cx, ids!(git_push_btn))
                    .set_visible(cx, has_remote && (status.ahead > 0 || !status.has_upstream));
                self.ui.button(cx, ids!(git_push_btn)).set_enabled(
                    cx,
                    (status.ahead > 0 || !status.has_upstream) && !self.git_operation_pending,
                );
                self.ui.button(cx, ids!(git_push_btn)).set_text(
                    cx,
                    if status.has_upstream {
                        "Push"
                    } else {
                        "Publish"
                    },
                );

                self.ui
                    .button(cx, ids!(git_pull_btn))
                    .set_visible(cx, has_remote && status.behind > 0);
                self.ui
                    .button(cx, ids!(git_pull_btn))
                    .set_enabled(cx, status.behind > 0 && !self.git_operation_pending);

                let has_github_remote = status
                    .remote
                    .as_deref()
                    .and_then(crate::git::github_repository)
                    .is_some();
                self.ui
                    .view(cx, ids!(git_pr_row))
                    .set_visible(cx, has_github_remote);
                self.ui
                    .button(cx, ids!(git_pr_btn))
                    .set_enabled(cx, status.pr_ready && !self.git_operation_pending);
            }
        }
        let tab = self.right_sidebar_tab;
        let show_git = tab == RightSidebarTab::Git;
        let show_tasks = tab == RightSidebarTab::Tasks;
        let show_file_tree = tab == RightSidebarTab::FileTree;
        let show_editor = tab == RightSidebarTab::Editor;

        let show_git_changes = show_git && !self.git_diff_open;
        let show_git_diff = show_git && self.git_diff_open;

        let sidebar_available = self.right_sidebar_available();
        let sidebar_visible = sidebar_available && self.right_sidebar_open;
        self.ui
            .view(cx, ids!(right_sidebar))
            .set_visible(cx, sidebar_visible);
        self.ui
            .view(cx, ids!(right_sidebar_resize_handle))
            .set_visible(cx, sidebar_visible);
        self.ui
            .button(cx, ids!(right_sidebar_toggle_btn))
            .set_visible(cx, sidebar_available);
        crate::components::nav_button::set_selected(
            cx,
            &self.ui.button(cx, ids!(right_sidebar_toggle_btn)),
            sidebar_visible,
        );
        let terminal_open = self
            .ui
            .project_terminal(cx, ids!(project_terminal))
            .is_open();
        crate::components::nav_button::set_selected(
            cx,
            &self.ui.button(cx, ids!(terminal_header_btn)),
            terminal_open,
        );
        self.ui
            .button(cx, ids!(right_sidebar_toggle_btn))
            .redraw(cx);
        self.ui.view(cx, ids!(header)).redraw(cx);

        if sidebar_visible {
            if let Some(mut sidebar) = self.ui.view(cx, ids!(right_sidebar)).borrow_mut() {
                sidebar.walk.width = Size::Fixed(self.right_sidebar_width);
                sidebar.redraw(cx);
            }
        }

        self.ui
            .view(cx, ids!(git_actions))
            .set_visible(cx, show_git);
        self.ui
            .view(cx, ids!(git_changes_header))
            .set_visible(cx, show_git_changes);
        self.ui
            .view(cx, ids!(git_changes_wrap))
            .set_visible(cx, show_git_changes);
        self.ui
            .view(cx, ids!(git_commit_section))
            .set_visible(cx, show_git_changes);
        self.ui
            .view(cx, ids!(git_diff_wrap))
            .set_visible(cx, show_git_diff);
        self.ui
            .view(cx, ids!(git_diff_loading))
            .set_visible(cx, show_git_diff && self.git_diff_pending);

        self.ui
            .view(cx, ids!(task_sidebar_wrap))
            .set_visible(cx, show_tasks);

        self.ui
            .view(cx, ids!(file_tree_wrap))
            .set_visible(cx, show_file_tree);
        self.ui
            .view(cx, ids!(code_editor_wrap))
            .set_visible(cx, show_editor);

        if show_editor {
            self.sync_code_editor_header(cx);
        }

        if show_file_tree {
            if let Some(mut tree) = self.ui.widget(cx, ids!(file_tree)).borrow_mut::<FileTree>() {
                tree.set_work_dir(cx, self.active_work_dir().map(Path::to_path_buf));
            }
        }

        self.ui
            .button(cx, ids!(tasks_tab_btn))
            .set_visible(cx, self.right_sidebar_agents_available);
        crate::components::nav_button::set_selected(
            cx,
            &self.ui.button(cx, ids!(tasks_tab_btn)),
            show_tasks,
        );
        // The editor tab only appears once a file has been opened, so the strip
        // does not show a control that would land on an empty panel.
        let editor_open = self
            .ui
            .code_editor_view(cx, ids!(code_editor_view))
            .is_open();
        self.ui
            .button(cx, ids!(code_editor_tab_btn))
            .set_visible(cx, editor_open);
        crate::components::nav_button::set_selected(
            cx,
            &self.ui.button(cx, ids!(code_editor_tab_btn)),
            show_editor,
        );
        crate::components::nav_button::set_selected(
            cx,
            &self.ui.button(cx, ids!(git_tab_btn)),
            tab == RightSidebarTab::Git,
        );
        crate::components::nav_button::set_selected(
            cx,
            &self.ui.button(cx, ids!(file_tree_tab_btn)),
            tab == RightSidebarTab::FileTree,
        );
    }

    fn set_right_sidebar_width(&mut self, cx: &mut Cx, width: f64) {
        self.right_sidebar_width =
            width.clamp(RIGHT_SIDEBAR_MIN_WIDTH, self.right_sidebar_max_width(cx));
        if let Some(mut sidebar) = self.ui.view(cx, ids!(right_sidebar)).borrow_mut() {
            sidebar.walk.width = Size::Fixed(self.right_sidebar_width);
            sidebar.redraw(cx);
        }
        self.ui
            .view(cx, ids!(right_sidebar_resize_handle))
            .redraw(cx);
    }

    fn content_row_width(&self, cx: &Cx) -> Option<f64> {
        let width = self.ui.view(cx, ids!(content_row)).area().rect(cx).size.x;
        (width > 0.0).then_some(width)
    }

    fn right_sidebar_max_width(&self, cx: &Cx) -> f64 {
        self.content_row_width(cx)
            .map(|width| self.right_sidebar_max_width_for(width))
            .unwrap_or(RIGHT_SIDEBAR_MAX_WIDTH)
    }

    fn right_sidebar_max_width_for(&self, content_width: f64) -> f64 {
        (content_width - RIGHT_SIDEBAR_MIN_MAIN_WIDTH - 10.0 - 6.0)
            .clamp(RIGHT_SIDEBAR_MIN_WIDTH, RIGHT_SIDEBAR_MAX_WIDTH)
    }

    fn right_sidebar_available(&self) -> bool {
        self.active_work_dir()
            .is_some_and(|work_dir| self.git_status.contains_key(work_dir))
            || (self.right_sidebar_agents_available && self.task_sidebar_open)
    }

    fn right_sidebar_is_visible(&self) -> bool {
        self.right_sidebar_open && self.right_sidebar_available()
    }

    fn sync_git_commit_button(&self, cx: &mut Cx) {
        let has_selection = self
            .ui
            .widget(cx, ids!(git_changes))
            .borrow::<GitChanges>()
            .is_some_and(|c| c.selected_count() > 0);
        let has_changes = self
            .active_work_dir()
            .and_then(|work_dir| self.git_status.get(work_dir))
            .is_some_and(|status| status.has_changes);
        let message = self.ui.text_input(cx, ids!(git_commit_message)).text();
        self.ui.button(cx, ids!(git_commit_btn)).set_enabled(
            cx,
            has_changes
                && has_selection
                && !message.trim().is_empty()
                && !self.git_operation_pending
                && !self.git_commit_message_pending
                && !self.git_diff_open,
        );
    }

    fn sync_git_selection_ui(&self, cx: &mut Cx) {
        let changes_widget = self.ui.widget(cx, ids!(git_changes));
        let Some(changes) = changes_widget.borrow::<GitChanges>() else {
            return;
        };
        let selected = changes.selected_count();
        let total = changes.file_count();
        drop(changes);
        self.ui
            .label(cx, ids!(git_selection_label))
            .set_text(cx, &format!("{selected}/{total} selected"));
        self.ui
            .button(cx, ids!(git_select_all_btn))
            .set_visible(cx, total > 0);
    }

    fn start_git_operation(
        &mut self,
        cx: &mut Cx,
        operation: String,
        task: impl FnOnce(&Path) -> Result<(), crate::git::GitError> + Send + 'static,
    ) {
        if self.git_operation_pending {
            return;
        }
        if self.git_commit_message_pending {
            self.cancel_git_commit_message_generation(cx);
        }
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            return;
        };
        let Some(tx) = self.tx.clone() else {
            return;
        };
        self.git_status_pending = false;
        self.git_operation_pending = true;
        self.git_diff_request_id = self.git_diff_request_id.wrapping_add(1);
        self.git_new_branch_open = false;
        self.git_diff_pending = false;
        self.git_diff_open = false;
        if operation == "commit"
            || operation == "push"
            || operation.starts_with("checkout ")
            || operation.starts_with("create branch ")
        {
            self.git_pr_created = false;
        }
        self.git_feedback = None;
        self.sync_right_sidebar(cx);
        self.next_git_request_id = self.next_git_request_id.wrapping_add(1);
        let request_id = self.next_git_request_id;
        self.git_operation_request_id = request_id;
        get_runtime().spawn_blocking(move || {
            let result = task(&work_dir).map_err(|error| error.message);
            let _ = tx.send(GuiAgentEvent::GitOperationFinished {
                request_id,
                work_dir,
                operation,
                result,
            });
            SignalToUI::set_ui_signal();
        });
    }

    fn start_git_diff(&mut self, cx: &mut Cx, path: String) {
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            return;
        };
        self.git_status_pending = false;
        self.git_diff_request_id = self.git_diff_request_id.wrapping_add(1);
        let request_id = self.git_diff_request_id;
        let Some(tx) = self.tx.clone() else {
            return;
        };
        self.git_diff_pending = true;
        self.git_diff_open = true;
        self.git_feedback = None;
        self.ui.label(cx, ids!(git_diff_path)).set_text(cx, &path);
        if let Some(mut diff_view) = self
            .ui
            .widget(cx, ids!(git_diff_text))
            .borrow_mut::<GitDiffView>()
        {
            diff_view.set_text(cx, "");
        }
        self.sync_right_sidebar(cx);
        get_runtime().spawn_blocking(move || {
            let result = crate::git::diff_file(&work_dir, &path).map_err(|error| error.message);
            let _ = tx.send(GuiAgentEvent::GitDiffLoaded {
                request_id,
                path,
                result,
            });
            SignalToUI::set_ui_signal();
        });
    }

    fn close_git_diff(&mut self, cx: &mut Cx) {
        self.git_status_pending = false;
        self.git_diff_request_id = self.git_diff_request_id.wrapping_add(1);
        self.git_diff_pending = false;
        self.git_diff_open = false;
        self.sync_right_sidebar(cx);
    }

    fn start_git_commit_message_generation(&mut self, cx: &mut Cx) {
        if self.git_commit_message_pending || self.git_operation_pending || self.git_diff_open {
            return;
        }
        if !self
            .ui
            .text_input(cx, ids!(git_commit_message))
            .text()
            .trim()
            .is_empty()
        {
            self.git_feedback = Some((
                false,
                "Clear the commit message before generating a new one.".to_owned(),
            ));
            self.sync_right_sidebar(cx);
            return;
        }
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            return;
        };
        let diff = match crate::git::commit_message_diff(&work_dir) {
            Ok(diff) => diff,
            Err(error) => {
                self.git_feedback = Some((false, error.message));
                self.sync_right_sidebar(cx);
                return;
            }
        };
        const MAX_COMMIT_MESSAGE_DIFF_CHARS: usize = 24_000;
        let diff = if diff.chars().count() > MAX_COMMIT_MESSAGE_DIFF_CHARS {
            format!(
                "{}\n\n[Diff truncated for message generation]",
                truncate_chars(&diff, MAX_COMMIT_MESSAGE_DIFF_CHARS)
            )
        } else {
            diff
        };
        let (api_key, account_id) = self.current_credentials(cx);
        let model = self
            .ui
            .icon_drop_down(cx, ids!(model_drop))
            .selected_label();
        let model = if model.trim().is_empty() {
            default_model_name().to_owned()
        } else {
            model
        };
        eprintln!("[commit_message_gen] Selected model: `{model}`");
        let has_antigravity_credentials =
            threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some();
        let has_opencode_credentials =
            threadlane_provider::opencode_auth::load_opencode_api_key().is_some();
        if let Some(error) = model_credential_error(
            &model,
            !api_key.is_empty(),
            has_antigravity_credentials,
            has_opencode_credentials,
        ) {
            self.git_feedback = Some((false, error.to_owned()));
            self.sync_right_sidebar(cx);
            return;
        }
        let Some(tx) = self.tx.clone() else {
            return;
        };
        self.git_commit_message_pending = true;
        self.git_feedback = None;
        self.git_commit_message_request_id = self.git_commit_message_request_id.wrapping_add(1);
        let request_id = self.git_commit_message_request_id;
        self.sync_right_sidebar(cx);
        let task = get_runtime().spawn(async move {
            let result = ProviderClient::new(api_key, account_id)
                .generate_commit_message(&model, &diff)
                .await;
            let _ = tx.send(GuiAgentEvent::GitCommitMessageGenerated {
                request_id,
                work_dir,
                result,
            });
            SignalToUI::set_ui_signal();
        });
        self.git_commit_message_abort = Some(task.abort_handle());
    }

    fn cancel_git_commit_message_generation(&mut self, cx: &mut Cx) {
        if let Some(abort) = self.git_commit_message_abort.take() {
            abort.abort();
        }
        self.git_commit_message_pending = false;
        self.git_commit_message_request_id = self.git_commit_message_request_id.wrapping_add(1);
        self.git_feedback = Some((true, "Commit message generation cancelled.".to_owned()));
        self.sync_right_sidebar(cx);
    }

    fn start_git_commit(&mut self, cx: &mut Cx, message: String) {
        let changes_widget = self.ui.widget(cx, ids!(git_changes));
        let Some(changes) = changes_widget.borrow::<GitChanges>() else {
            return;
        };
        let selected_paths = changes.selected_files();
        let all_paths = changes.all_files();
        if selected_paths.is_empty() {
            self.git_feedback = Some((false, "Select at least one file to commit.".to_owned()));
            self.sync_right_sidebar(cx);
            return;
        }
        drop(changes);

        self.start_git_operation(cx, "commit".to_owned(), move |work_dir| {
            for path in &selected_paths {
                crate::git::stage_file(work_dir, path)?;
            }
            for path in &all_paths {
                if !selected_paths.contains(path) {
                    crate::git::unstage_file(work_dir, path)?;
                }
            }
            crate::git::commit_staged(work_dir, &message)
        });
    }

    fn start_git_push(&mut self, cx: &mut Cx) {
        self.start_git_operation(cx, "push".to_owned(), crate::git::push);
    }

    fn start_git_pull(&mut self, cx: &mut Cx) {
        self.start_git_operation(cx, "pull".to_owned(), crate::git::pull);
    }

    fn start_git_create_branch(&mut self, cx: &mut Cx, name: String) {
        self.git_new_branch_open = false;
        self.ui
            .text_input(cx, ids!(git_branch_dialog_name))
            .set_text(cx, "");
        self.start_git_operation(cx, format!("create branch `{name}`"), move |work_dir| {
            crate::git::create_branch(work_dir, &name)
        });
    }

    fn open_github_pull_request_in_browser(&mut self, cx: &mut Cx) {
        let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) else {
            return;
        };
        let Some(status) = self.git_status.get(&work_dir).cloned() else {
            self.git_feedback = Some((false, "This project is not a Git repository.".to_owned()));
            self.sync_right_sidebar(cx);
            return;
        };
        let Some(remote) = status.remote else {
            self.git_feedback = Some((
                false,
                "No origin remote is configured for this project.".to_owned(),
            ));
            self.sync_right_sidebar(cx);
            return;
        };
        let Some(head) = status.branch else {
            self.git_feedback = Some((false, "Pull requests require a named branch.".to_owned()));
            self.sync_right_sidebar(cx);
            return;
        };
        let base = crate::git::default_branch(&work_dir);
        let Some(url) = crate::git::github_compare_url(&remote, &head, base.as_deref()) else {
            self.git_feedback = Some((
                false,
                "Origin remote is not a GitHub repository.".to_owned(),
            ));
            self.sync_right_sidebar(cx);
            return;
        };
        crate::git::open_browser_url(cx, &url);
        self.git_feedback = Some((true, "Opened Pull Request link in browser.".to_owned()));
        self.sync_right_sidebar(cx);
    }

    fn checkout_git_branch(&mut self, cx: &mut Cx, branch: String) {
        self.start_git_operation(cx, format!("checkout `{branch}`"), move |work_dir| {
            crate::git::checkout(work_dir, &branch)
        });
    }

    fn sync_task_sidebar(&mut self, cx: &mut Cx) {
        let active_key = self.workspace_state.active_key().cloned();
        let records = active_key.as_ref().and_then(|key| {
            let work_dir =
                std::fs::canonicalize(&key.work_dir).unwrap_or_else(|_| key.work_dir.clone());
            let project_id = self.supervisor_projects.get(&work_dir)?;
            Some((
                work_dir,
                self.supervisor.as_ref()?.list_tasks_for_project(project_id),
            ))
        });
        let items = records
            .map(|(work_dir, records)| {
                task_sidebar_items(records, |record| {
                    record
                        .session_file
                        .as_deref()
                        .and_then(|path| session_entry_for_file(&work_dir, path))
                        .map(|entry| entry.title)
                })
            })
            .unwrap_or_default();
        let plan = active_key
            .as_ref()
            .and_then(|key| self.session_runtimes.get(key))
            .map(|runtime| runtime.plan.clone())
            .unwrap_or_default();
        let (visible, _count) = task_header_state(&plan, &items);
        self.right_sidebar_agents_available = visible;
        if !visible && self.right_sidebar_tab == RightSidebarTab::Tasks {
            self.task_sidebar_open = false;
        }
        if let Some(mut sidebar) = self
            .ui
            .widget(cx, ids!(task_sidebar))
            .borrow_mut::<TaskSidebar>()
        {
            sidebar.set_content(
                cx,
                plan,
                items,
                active_key.as_ref().map(|key| key.session_id.clone()),
            );
        }
        self.sync_right_sidebar(cx);
    }

    fn observe_session_task_event(
        &self,
        work_dir: &Path,
        session_id: &str,
        session_file: Option<&Path>,
        event: &AgentEvent,
    ) -> bool {
        let canonical = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        let Some(project_id) = self.supervisor_projects.get(&canonical) else {
            return false;
        };
        self.supervisor.as_ref().is_some_and(|supervisor| {
            supervisor.observe_session_event(project_id, session_id, session_file, event)
        })
    }

    fn finish_session_tasks(&self, work_dir: &Path, session_id: &str) -> bool {
        let canonical = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        let Some(project_id) = self.supervisor_projects.get(&canonical) else {
            return false;
        };
        self.supervisor
            .as_ref()
            .is_some_and(|supervisor| supervisor.finish_session_tasks(project_id, session_id))
    }

    fn is_attached_project(&self, work_dir: &Path) -> bool {
        if let Some(registry) = self.project_registry.as_ref() {
            return registry
                .projects()
                .iter()
                .any(|project| project.path == work_dir && project.path.is_dir());
        }
        crate::panels::sessions::state::SESSIONS_DATA
            .read()
            .unwrap()
            .projects
            .iter()
            .any(|project| project.work_dir == work_dir && project.available)
    }

    fn open_project_picker(&self) {
        // rfd's macOS backend must be invoked from the application main thread.
        // Makepad action handlers run there, so do not move this call to a worker.
        let picked = rfd::FileDialog::new()
            .set_title("Attach a project folder")
            .pick_folder();
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(GuiAgentEvent::ProjectFolderPicked(Ok(picked)));
            SignalToUI::set_ui_signal();
        }
    }

    fn apply_project_folder_result(
        &mut self,
        cx: &mut Cx,
        result: Result<Option<PathBuf>, String>,
    ) {
        let raw_path = match result {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(error) => {
                self.push_chat(MsgRole::System, format!("Project picker failed: {error}"));
                return;
            }
        };
        let Some(registry) = self.project_registry.as_mut() else {
            self.push_chat(MsgRole::System, "The project registry is unavailable.");
            return;
        };
        match registry.attach(&raw_path) {
            Ok(project) => {
                if let Some(supervisor) = &self.supervisor {
                    if let Ok(record) = supervisor.register_project(&project.path) {
                        let path = std::fs::canonicalize(&project.path)
                            .unwrap_or_else(|_| project.path.clone());
                        self.supervisor_projects.insert(path, record.id);
                    }
                }
                self.refresh_registered_sessions();
                self.select_project_draft(cx, project.path);
                self.ui
                    .threadlane_command_text_input(cx, ids!(prompt_input))
                    .text_input_ref(cx)
                    .set_key_focus(cx);
            }
            Err(error) => self.push_chat(
                MsgRole::System,
                format!("Could not attach project: {error}"),
            ),
        }
    }

    fn detach_project(&mut self, cx: &mut Cx, work_dir: PathBuf) {
        if is_project_working(&work_dir) {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Stop all running sessions in `{}` before detaching it.",
                    project_name(&work_dir)
                ),
            );
            return;
        }
        let Some(registry) = self.project_registry.as_mut() else {
            return;
        };
        match registry.detach(&work_dir) {
            Ok(true) => {
                let was_active = self.active_work_dir() == Some(work_dir.as_path());
                self.session_runtimes
                    .retain(|key, _| key.work_dir != work_dir);
                if let Some(group) = self
                    .project_terminals
                    .remove(&canonical_terminal_work_dir(&work_dir))
                {
                    group.terminate();
                }
                let keys = self
                    .workspace_state
                    .keys_for_project(&work_dir)
                    .cloned()
                    .collect::<Vec<_>>();
                for key in keys {
                    self.workspace_state.remove(&key);
                }
                self.refresh_registered_sessions();
                if was_active {
                    if let Some(fallback) = self.registered_project_dirs().into_iter().next() {
                        self.select_project_draft(cx, fallback);
                    }
                }
            }
            Ok(false) => {}
            Err(error) => self.push_chat(
                MsgRole::System,
                format!("Could not detach project: {error}"),
            ),
        }
    }

    fn current_credentials(&self, cx: &Cx) -> (String, Option<String>) {
        let mut api_key = self.ui.text_input(cx, ids!(api_key_input)).text();
        let mut account_id = None;

        if let Some(creds) = auth::load_credentials() {
            if api_key.trim().is_empty() || api_key.trim() == creds.access_token {
                api_key = creds.access_token;
                account_id = creds.account_id;
            }
        }
        if api_key.trim().is_empty() {
            api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        }

        (api_key, account_id)
    }

    fn start_background_task(
        &mut self,
        cx: &mut Cx,
        work_dir: PathBuf,
        prompt: String,
        api_key: String,
        account_id: Option<String>,
        model: String,
    ) {
        if prompt.is_empty() {
            self.push_chat(MsgRole::System, "Usage: /task <prompt>");
            return;
        }
        let Some(supervisor) = self.supervisor.clone() else {
            self.push_chat(MsgRole::System, "Background task service is unavailable.");
            return;
        };
        let canonical = std::fs::canonicalize(&work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        let project_id = match self.supervisor_projects.get(&canonical) {
            Some(project_id) => project_id.clone(),
            None => match supervisor.register_project(&canonical) {
                Ok(project) => {
                    self.supervisor_projects
                        .insert(canonical.clone(), project.id.clone());
                    project.id
                }
                Err(error) => {
                    self.push_chat(
                        MsgRole::System,
                        format!("Could not register background task project: {error}"),
                    );
                    return;
                }
            },
        };
        let task_id = match supervisor.create_task(
            &project_id,
            None,
            CodingAgentOptions {
                api_key,
                account_id,
                model,
                work_dir: canonical.clone(),
                session_file: None,
                system_prompt: Default::default(),
            },
        ) {
            Ok(task_id) => task_id,
            Err(error) => {
                self.push_chat(
                    MsgRole::System,
                    format!("Could not create background task: {error}"),
                );
                return;
            }
        };
        if let Err(error) = supervisor.submit_input(&task_id, prompt) {
            let _ = supervisor.cancel_task(&task_id);
            self.push_chat(
                MsgRole::System,
                format!("Could not start background task: {error}"),
            );
        } else {
            self.push_chat(
                MsgRole::System,
                format!("Started background task {task_id}."),
            );
        }
        self.sync_task_sidebar(cx);
        cx.redraw_all();
    }

    fn refresh_project_capabilities(&mut self, cx: &mut Cx, work_dir: &Path) {
        let canonical = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        let capabilities = if let Some(cached) = self.capability_cache.get(&canonical) {
            cached.clone()
        } else {
            let (api_key, account_id) = self.current_credentials(cx);
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key,
                account_id,
                model: "gpt-5.6-luna".to_string(),
                work_dir: canonical.clone(),
                session_file: None,
                system_prompt: Default::default(),
            });
            let skills = agent
                .skills
                .list_skills()
                .into_iter()
                .filter(|skill| skill.enabled && skill.is_valid)
                .collect::<Vec<_>>();
            let agents = discover_agents(&canonical, AgentScope::Both).agents;
            let mut commands = builtin_commands();
            commands.extend(skills.iter().map(|skill| CommandInfo {
                name: format!("skill {}", skill.id),
                description: format!(
                    "{} · {}",
                    skill.scope.display_name(),
                    truncate_chars(&normalize_catalog_text(&skill.description), 120)
                ),
            }));
            for manifest in agent.wasi_extensions.extension_manifests() {
                commands.extend(manifest.commands.into_iter().map(|command| CommandInfo {
                    name: command.name,
                    description: command.description,
                }));
            }
            let capabilities = ProjectCapabilities {
                summary: format_capabilities_summary(&skills, &agents),
                button_text: format_capabilities_button_text(skills.len(), agents.len()),
                commands,
            };
            self.capability_cache
                .insert(canonical.clone(), capabilities.clone());
            capabilities
        };
        self.capabilities_summary = capabilities.summary;
        self.commands = capabilities.commands;
        self.ui
            .button(cx, ids!(caps_btn))
            .set_text(cx, &capabilities.button_text);
    }

    fn save_active_draft(&mut self, cx: &Cx) {
        let draft = self
            .ui
            .threadlane_command_text_input(cx, ids!(prompt_input))
            .text_input_ref(cx)
            .text();
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            workspace.ui.draft = draft;
        }
    }

    fn push_chat(&mut self, role: MsgRole, text: impl Into<String>) {
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            workspace.chat.push_chat(role, text);
        }
    }

    fn push_chat_to(&mut self, key: SessionKey, role: MsgRole, text: impl Into<String>) {
        self.workspace_state
            .workspace_mut(key)
            .chat
            .push_chat(role, text);
    }

    fn set_status(&mut self, cx: &mut Cx, status: UiStatus, text: &str) {
        if let Some(key) = self.workspace_state.active_key().cloned() {
            self.set_session_status(cx, &key, status, text);
        } else {
            self.apply_status_ui(cx, status, text);
        }
    }

    fn set_session_status(&mut self, cx: &mut Cx, key: &SessionKey, status: UiStatus, text: &str) {
        if let Some(runtime) = self.session_runtimes.get_mut(key) {
            runtime.status = status;
            runtime.status_text = text.to_string();
        }
        set_session_working(&key.work_dir, &key.session_id, status == UiStatus::Working);
        self.ui.widget(cx, ids!(session_list)).redraw(cx);
        if self.workspace_state.is_active(key) {
            self.apply_status_ui(cx, status, text);
        }
    }

    fn restore_active_status(&mut self, cx: &mut Cx) {
        let Some(key) = self.workspace_state.active_key().cloned() else {
            self.apply_status_ui(cx, UiStatus::Ready, "Ready");
            return;
        };
        let (status, text) = self
            .session_runtimes
            .get(&key)
            .map(|runtime| (runtime.status, runtime.status_text.clone()))
            .unwrap_or((UiStatus::Ready, String::new()));
        self.apply_status_ui(cx, status, &text);
    }

    fn apply_status_ui(&mut self, cx: &mut Cx, status: UiStatus, text: &str) {
        let composer_status = match status {
            UiStatus::Ready => ComposerStatus::Ready,
            UiStatus::Working => ComposerStatus::Working,
            UiStatus::Error => ComposerStatus::Error,
        };
        self.composer_state.set_status(composer_status, text);
        self.ui
            .label(cx, ids!(chat_working_label))
            .set_text(cx, text);
        self.ui.label(cx, ids!(chat_working_label)).redraw(cx);
        self.busy = status == UiStatus::Working;
        let working = status == UiStatus::Working;
        self.ui
            .widget(cx, ids!(chat_working_indicator))
            .set_visible(cx, working);
        self.ui.widget(cx, ids!(chat_working_indicator)).redraw(cx);
        self.apply_composer_presentation(cx);
    }

    fn set_live_composer_activity(&mut self, cx: &mut Cx, key: &SessionKey, text: &str) {
        if !self.workspace_state.is_active(key) {
            return;
        }
        self.ui
            .label(cx, ids!(chat_working_label))
            .set_text(cx, text);
        self.ui.label(cx, ids!(chat_working_label)).redraw(cx);
    }

    fn apply_composer_presentation(&mut self, cx: &mut Cx) {
        let presentation = self.composer_state.presentation();
        self.ui
            .widget(cx, ids!(effort_picker))
            .set_visible(cx, presentation.show_model);
        self.ui
            .widget(cx, ids!(model_picker))
            .set_visible(cx, presentation.show_model);

        self.ui
            .button(cx, ids!(attach_btn))
            .set_visible(cx, presentation.show_attach);
        let has_generation = self
            .workspace_state
            .active_key()
            .and_then(|key| self.session_runtimes.get(key))
            .is_some_and(|runtime| runtime.generation.is_some());
        let show_stop = presentation.show_stop(has_generation);
        self.ui
            .button(cx, ids!(send_btn))
            .set_visible(cx, !show_stop);
        self.ui
            .button(cx, ids!(stop_btn))
            .set_visible(cx, show_stop);
    }

    fn apply_session_context_action(
        &mut self,
        cx: &mut Cx,
        action: fn(&SessionEntry) -> bool,
        action_name: &str,
    ) {
        let Some(entry) = self.session_context_entry.take() else {
            return;
        };
        if let Some(mut menu) = self
            .ui
            .widget(cx, ids!(session_context_menu))
            .borrow_mut::<SessionContextMenu>()
        {
            menu.close(cx);
        }

        if is_session_working(&entry.work_dir, &entry.id) {
            self.push_chat(
                MsgRole::System,
                format!("Stop session `{}` before modifying it.", entry.title),
            );
            cx.redraw_all();
            return;
        }

        if !action(&entry) {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Could not {} session `{}`.",
                    action_name.to_lowercase(),
                    entry.title
                ),
            );
            cx.redraw_all();
            return;
        }
        let was_active = active_session_entry()
            .is_some_and(|active| active.id == entry.id && active.work_dir == entry.work_dir);
        let removed_key = SessionKey::new(entry.work_dir.clone(), entry.id.clone());
        self.workspace_state.remove(&removed_key);
        self.session_runtimes.remove(&removed_key);
        self.refresh_registered_sessions();

        if was_active {
            let fallback = {
                let data = crate::panels::sessions::state::SESSIONS_DATA
                    .read()
                    .unwrap();
                data.projects
                    .iter()
                    .find(|project| project.work_dir == entry.work_dir)
                    .and_then(|project| project.sessions.first())
                    .cloned()
            };
            if let Some(fallback) = fallback {
                self.activate_session(cx, fallback);
                return;
            }
            self.select_project_draft(cx, entry.work_dir.clone());
        }
        self.push_chat(
            MsgRole::System,
            format!("{} session `{}`.", action_name, entry.title),
        );
        cx.redraw_all();
    }

    fn create_and_activate_session(&mut self, cx: &mut Cx, work_dir: PathBuf) {
        let Some(entry) = create_new_session(&work_dir) else {
            self.push_chat(MsgRole::System, "Could not create a new session file.");
            cx.redraw_all();
            return;
        };
        self.refresh_registered_sessions();
        self.activate_session(cx, entry);
    }

    fn activate_session(&mut self, cx: &mut Cx, mut entry: SessionEntry) {
        entry.work_dir =
            std::fs::canonicalize(&entry.work_dir).unwrap_or_else(|_| entry.work_dir.clone());
        if !self.is_attached_project(&entry.work_dir) {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Attach `{}` before opening its sessions.",
                    entry.work_dir.display()
                ),
            );
            cx.redraw_all();
            return;
        }

        let key = SessionKey::new(entry.work_dir.clone(), entry.id.clone());
        self.select_workspace_ui(cx, entry.work_dir.clone(), entry.id.clone());
        set_active_session(&entry.work_dir, &entry.id);
        if let Some(registry) = self.project_registry.as_mut() {
            if let Err(error) = registry.remember_selection(&entry.work_dir, Some(&entry.id)) {
                self.push_chat_to(
                    key.clone(),
                    MsgRole::System,
                    format!("Could not update recent-project state: {error}"),
                );
            }
        }

        if !self.session_runtimes.contains_key(&key) {
            let (api_key, account_id) = self.current_credentials(cx);
            let selected_model = self
                .ui
                .icon_drop_down(cx, ids!(model_drop))
                .selected_label();
            let model = if selected_model.is_empty() {
                default_model_name().to_string()
            } else {
                selected_model
            };
            let reasoning_effort = ReasoningEffort::from_label(
                &self
                    .ui
                    .icon_drop_down(cx, ids!(effort_drop))
                    .selected_label(),
            )
            .unwrap_or_default();
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key,
                account_id,
                model: model.clone(),
                work_dir: entry.work_dir.clone(),
                session_file: Some(entry.session_file.clone()),
                system_prompt: Default::default(),
            });
            let startup_error = agent.harness_error().map(str::to_owned);
            let model = agent.session_tree.model.clone().unwrap_or(model);
            let latest_usage = agent
                .session_tree
                .get_fact(CONTEXT_USAGE_FACT)
                .and_then(|value| serde_json::from_str::<TokenUsage>(value).ok());
            let messages = agent.session_tree.get_active_branch_messages();
            let mut runtime = SessionRuntime::new(agent, model, reasoning_effort);
            runtime.latest_usage = latest_usage;
            self.session_runtimes.insert(key.clone(), runtime);
            self.workspace_state
                .workspace_mut(key.clone())
                .chat
                .replace_from_agent_messages(&messages);
            if let Some(error) = startup_error {
                self.workspace_state
                    .workspace_mut(key.clone())
                    .chat
                    .push_chat(MsgRole::System, format!("Session unavailable: {error}"));
            }
            let activities = restore_harness_activities(&entry.session_file);
            let health = session_health(&activities);
            self.workspace_state
                .workspace_mut(key.clone())
                .chat
                .harness_activities = activities;
            set_session_health(&entry.work_dir, &entry.id, health);
        }

        if let Some((model, reasoning_effort)) = self
            .session_runtimes
            .get(&key)
            .map(|runtime| (runtime.model.clone(), runtime.reasoning_effort))
        {
            self.set_model_dropup_options(cx, self.available_models.clone(), &model);
            self.set_reasoning_effort_picker(cx, reasoning_effort);
        }

        self.refresh_project_capabilities(cx, &entry.work_dir);
        self.restore_active_status(cx);
        self.sync_task_sidebar(cx);
        self.sync_context_window(cx);
        cx.redraw_all();
    }

    fn build_cmd_items(&mut self, cx: &mut Cx) {
        let cti = self
            .ui
            .threadlane_command_text_input(cx, ids!(prompt_input));
        let search = cti.search_text(cx).to_lowercase();
        let commands = self
            .commands
            .iter()
            .filter(|cmd| search.is_empty() || cmd.name.to_lowercase().starts_with(&search))
            .cloned()
            .collect();
        cti.set_items(cx, commands);
    }

    fn dispatch_input(&mut self, cx: &mut Cx, input_text: String, origin: InputOrigin) {
        let consumes_composer = origin.consumes_composer();
        match input_text.trim() {
            "/login antigravity" => {
                self.start_antigravity_login(cx);
                return;
            }
            "/antigravity.doctor" | "/doctor" => {
                self.start_antigravity_doctor(cx);
                return;
            }
            "/task" => {
                self.push_chat(MsgRole::System, "Usage: /task <prompt>");
                cx.redraw_all();
                return;
            }
            _ => {}
        }

        let attachments = if consumes_composer {
            self.workspace_state
                .active_workspace()
                .map(|workspace| workspace.ui.attachments.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let (api_key, account_id) = self.current_credentials(cx);
        let selected_model = self
            .ui
            .icon_drop_down(cx, ids!(model_drop))
            .selected_label();
        let model_name = if selected_model.is_empty() {
            default_model_name().to_string()
        } else {
            selected_model
        };
        let has_antigravity_credentials =
            threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some();
        let has_opencode_credentials =
            threadlane_provider::opencode_auth::load_opencode_api_key().is_some();
        if let Some(error) = model_credential_error(
            &model_name,
            !api_key.is_empty(),
            has_antigravity_credentials,
            has_opencode_credentials,
        ) {
            self.push_chat(MsgRole::System, error);
            cx.redraw_all();
            return;
        }
        let reasoning_effort = ReasoningEffort::from_label(
            &self
                .ui
                .icon_drop_down(cx, ids!(effort_drop))
                .selected_label(),
        )
        .unwrap_or_default();
        let Some(active_key) = self.workspace_state.active_key().cloned() else {
            return;
        };
        let work_dir = active_key.work_dir.clone();

        if let Some(prompt) = input_text.trim().strip_prefix("/task ") {
            self.start_background_task(
                cx,
                work_dir,
                prompt.trim().to_string(),
                api_key,
                account_id,
                model_name,
            );
            if consumes_composer {
                if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                    clear_composer_for_dispatch(origin, &mut workspace.ui);
                }
                self.refresh_attachment_ui(cx);
                self.ui
                    .threadlane_command_text_input(cx, ids!(prompt_input))
                    .reset(cx);
            }
            return;
        }

        if consumes_composer
            && active_session_entry().is_none()
            && input_text.trim_start().starts_with('/')
        {
            self.push_chat(
                MsgRole::System,
                "Select an existing session before running a session command.",
            );
            cx.redraw_all();
            return;
        }

        if consumes_composer && active_key.is_draft() {
            self.save_active_draft(cx);
            let Some(entry) = create_new_session(&work_dir) else {
                self.push_chat(MsgRole::System, "Could not create a new session file.");
                cx.redraw_all();
                return;
            };
            self.refresh_registered_sessions();
            set_active_session(&entry.work_dir, &entry.id);
            let key = SessionKey::new(entry.work_dir.clone(), entry.id);
            self.workspace_state
                .move_workspace(&active_key, key.clone());
            self.select_workspace_ui(cx, entry.work_dir.clone(), key.session_id.clone());
            self.session_runtimes.remove(&active_key);
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key: api_key.clone(),
                account_id: account_id.clone(),
                model: model_name.clone(),
                work_dir: entry.work_dir,
                session_file: Some(entry.session_file),
                system_prompt: Default::default(),
            });
            self.session_runtimes.insert(
                key,
                SessionRuntime::new(agent, model_name.clone(), reasoning_effort),
            );
        }

        let Some(key) = self.workspace_state.active_key().cloned() else {
            return;
        };
        if !self.session_runtimes.contains_key(&key) {
            let session_file = active_session_entry().map(|entry| entry.session_file);
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key: api_key.clone(),
                account_id: account_id.clone(),
                model: model_name.clone(),
                work_dir: key.work_dir.clone(),
                session_file,
                system_prompt: Default::default(),
            });
            self.session_runtimes.insert(
                key.clone(),
                SessionRuntime::new(agent, model_name.clone(), reasoning_effort),
            );
        }

        if let Some(runtime) = self.session_runtimes.get_mut(&key) {
            runtime.reasoning_effort = reasoning_effort;
        }

        if let Some(model) = input_text.trim().strip_prefix("/model ") {
            if !model.trim().is_empty() {
                if let Some(runtime) = self.session_runtimes.get_mut(&key) {
                    runtime.model = model.trim().to_string();
                }
            }
        }

        let Some(tx) = self.tx.clone() else { return };
        let agent_arc = self.session_runtimes[&key].agent.clone();
        let (submitted_draft, input_str) = match submitted_draft(&input_text) {
            Some(draft) => draft,
            None if !attachments.is_empty() => (input_text.clone(), String::new()),
            None => return,
        };
        if consumes_composer
            && !input_str.trim().is_empty()
            && !threadlane_provider::router::is_antigravity_model(&model_name)
        {
            if let Some(entry) = active_session_entry() {
                self.spawn_session_title(
                    entry,
                    input_str.clone(),
                    api_key.clone(),
                    account_id.clone(),
                    model_name.clone(),
                );
            }
        }
        self.next_generation_id = self.next_generation_id.wrapping_add(1);
        let generation_id = self.next_generation_id;

        if consumes_composer {
            let attachment_names =
                attachments
                    .iter()
                    .enumerate()
                    .fold(String::new(), |mut acc, (i, attachment)| {
                        if i > 0 {
                            acc.push_str(", ");
                        }
                        acc.push_str(&attachment.display_name);
                        acc
                    });
            let visible_input = if attachment_names.is_empty() {
                input_str.clone()
            } else if input_str.is_empty() {
                format!("Attached: {attachment_names}")
            } else {
                format!("{input_str}\n\nAttached: {attachment_names}")
            };
            self.push_chat(MsgRole::User, visible_input);
        }
        let chat_list = self.ui.widget(cx, ids!(chat_list));
        chat_list.portal_list(cx, ids!(list)).set_tail_range(true);
        cx.redraw_all();

        let event_work_dir = key.work_dir.clone();
        let event_session_id = key.session_id.clone();
        let generation_attachments = attachments.clone();

        // An ACP model routes the turn to an external agent instead of the
        // built-in loop. Its updates are mapped onto the same `AgentEvent`
        // stream, so the transcript renders identically either way.
        if let Some(agent_id) = threadlane_coding_agent::acp_agent_id(&model_name) {
            self.start_acp_generation(
                cx,
                key.clone(),
                generation_id,
                agent_id.to_string(),
                input_str,
                consumes_composer,
                submitted_draft,
                attachments,
                origin,
            );
            return;
        }

        let generation_handle = get_runtime().spawn(async move {
            let mut agent_lock = agent_arc.lock().await;
            agent_lock.set_reasoning_effort(reasoning_effort).await;

            // Poll input and its event stream in one task. This keeps event
            // forwarding scoped to the generation and preserves terminal order.
            let mut event_rx = agent_lock.subscribe();
            let mut harness_watch = agent_lock.watch_harness().ok().flatten();
            let mut harness_tick = tokio::time::interval(tokio::time::Duration::from_millis(50));
            harness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            if let Some(watch) = harness_watch.as_ref() {
                let _ = tx.send(GuiAgentEvent::HarnessSnapshot {
                    generation_id,
                    work_dir: event_work_dir.clone(),
                    session_id: event_session_id.clone(),
                    snapshot: watch.snapshot().clone(),
                });
                SignalToUI::set_ui_signal();
            }
            let input_future =
                agent_lock.handle_input_with_images(&input_str, generation_attachments);
            tokio::pin!(input_future);
            let output = loop {
                tokio::select! {
                    output = &mut input_future => break output,
                    result = event_rx.recv() => match result {
                        Ok(event) => {
                            let _ = tx.send(GuiAgentEvent::GenerationAgent {
                                generation_id,
                                work_dir: event_work_dir.clone(),
                                session_id: event_session_id.clone(),
                                event,
                            });
                            SignalToUI::set_ui_signal();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break None,
                    },
                    _ = harness_tick.tick() => {
                        if let Some(watch) = harness_watch.as_mut() {
                            if let Ok(events) = watch.poll() {
                                for event in events {
                                    let _ = tx.send(GuiAgentEvent::HarnessEvent {
                                        generation_id,
                                        work_dir: event_work_dir.clone(),
                                        session_id: event_session_id.clone(),
                                        event,
                                    });
                                    SignalToUI::set_ui_signal();
                                }
                            }
                        }
                    }
                }
            };
            while let Ok(event) = event_rx.try_recv() {
                let _ = tx.send(GuiAgentEvent::GenerationAgent {
                    generation_id,
                    work_dir: event_work_dir.clone(),
                    session_id: event_session_id.clone(),
                    event,
                });
                SignalToUI::set_ui_signal();
            }
            if let Some(watch) = harness_watch.as_mut() {
                if let Ok(events) = watch.poll() {
                    for event in events {
                        let _ = tx.send(GuiAgentEvent::HarnessEvent {
                            generation_id,
                            work_dir: event_work_dir.clone(),
                            session_id: event_session_id.clone(),
                            event,
                        });
                        SignalToUI::set_ui_signal();
                    }
                }
            }

            if let Some(out) = output {
                let _ = tx.send(GuiAgentEvent::CommandOutput {
                    generation_id,
                    work_dir: event_work_dir.clone(),
                    session_id: event_session_id.clone(),
                    output: out.unwrap_or_else(|error| error),
                });
                SignalToUI::set_ui_signal();
            }
            let _ = tx.send(GuiAgentEvent::GenerationFinished {
                generation_id,
                work_dir: event_work_dir,
                session_id: event_session_id,
            });
            SignalToUI::set_ui_signal();
        });
        if let Some(runtime) = self.session_runtimes.get_mut(&key) {
            runtime.generation = Some(GenerationRun {
                id: generation_id,
                handle: generation_handle,
            });
            runtime.terminal_generation_id = None;
            if consumes_composer {
                runtime.submitted_draft = Some((generation_id, submitted_draft));
                runtime.submitted_attachments = Some((generation_id, attachments));
            } else {
                runtime.submitted_draft = None;
                runtime.submitted_attachments = None;
            }
        }
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            clear_composer_for_dispatch(origin, &mut workspace.ui);
        }
        if consumes_composer {
            self.refresh_attachment_ui(cx);
            self.ui
                .threadlane_command_text_input(cx, ids!(prompt_input))
                .reset(cx);
        }
        self.set_session_status(cx, &key, UiStatus::Working, "Working...");
    }

    fn spawn_session_title(
        &self,
        entry: SessionEntry,
        prompt: String,
        api_key: String,
        account_id: Option<String>,
        model: String,
    ) {
        let work_dir = entry.work_dir.clone();
        let session_id = entry.id.clone();
        let path = entry.session_file.clone();
        let mut tree = match threadlane_agent::SessionTree::load_from_file(&path) {
            Ok(tree) => tree,
            Err(error) => {
                eprintln!(
                    "warning: unable to load session {} for automatic title generation ({}): {}",
                    session_id,
                    path.display(),
                    error
                );
                return;
            }
        };
        let title_prompt = title_prompt_for_submission(&tree, Some(&prompt));
        if !session_title_eligible(&tree, Some(&prompt)) {
            return;
        }
        let Some(title_prompt) = title_prompt else {
            return;
        };
        // The durable marker is written before the detached provider task is spawned.
        let title_attempted = match tree.mark_title_attempted() {
            Ok(title_attempted) => title_attempted,
            Err(error) => {
                eprintln!(
                    "warning: unable to persist automatic title attempt for session {}: {}",
                    session_id, error
                );
                return;
            }
        };
        if !title_attempted || !begin_title_generation(&work_dir, &session_id) {
            return;
        }
        let Some(tx) = self.tx.clone() else {
            eprintln!(
                "warning: automatic session title generation unavailable: UI channel is closed"
            );
            end_title_generation(&work_dir, &session_id);
            return;
        };
        get_runtime().spawn(async move {
            let result = async {
                let client = ProviderClient::new(api_key, account_id);
                let raw = client.generate_title(&model, &title_prompt).await?;
                let title = normalize_session_title(&raw);
                if title.is_empty() {
                    return Err("title normalization produced an empty title".to_string());
                }
                let mut tree = threadlane_agent::SessionTree::load_from_file(&path)
                    .map_err(|error| format!("reload failed: {error}"))?;
                if tree.has_name() {
                    return Err("session was named while title generation was running".to_string());
                }
                tree.set_name(title)
                    .map_err(|error| format!("persistence failed: {error}"))?;
                Ok::<(), String>(())
            }
            .await;
            if let Err(error) = &result {
                eprintln!(
                    "warning: automatic title generation failed for session {}: {}",
                    session_id, error
                );
            }
            end_title_generation(&work_dir, &session_id);
            if result.is_ok() {
                let _ = tx.send(GuiAgentEvent::SessionTitleGenerated);
                SignalToUI::set_ui_signal();
            }
        });
    }

    fn spawn_model_fetch(&self, api_key: String, account_id: Option<String>) {
        if api_key.is_empty() {
            return;
        }
        let Some(tx) = self.tx.clone() else { return };
        get_runtime().spawn(async move {
            let models = fetch_available_models(&api_key, account_id.as_deref()).await;
            let _ = tx.send(GuiAgentEvent::AvailableModelsLoaded(models));
            SignalToUI::set_ui_signal();
        });
    }

    fn handle_agent_event(
        &mut self,
        cx: &mut Cx,
        event: AgentEvent,
        generation: Option<(SessionKey, u64)>,
    ) {
        let target_key = generation
            .as_ref()
            .map(|(key, _)| key.clone())
            .or_else(|| self.workspace_state.active_key().cloned());

        match event {
            AgentEvent::AgentStart => {
                if let Some(key) = target_key {
                    self.set_session_status(cx, &key, UiStatus::Working, "Working...");
                    self.set_live_composer_activity(cx, &key, "Thinking");
                    set_live_main_activity(
                        &mut self.workspace_state.workspace_mut(key).chat,
                        "Thinking",
                    );
                }
            }
            AgentEvent::MessageUpdate {
                text_delta,
                reasoning_delta,
                tool_call_name,
            } => {
                let Some(key) = target_key else { return };
                let workspace = self.workspace_state.workspace_mut(key.clone());
                let activity_detail = tool_call_name
                    .as_ref()
                    .map(|name| format!("Preparing tool: {name}"))
                    .or_else(|| reasoning_delta.as_ref().map(|_| "Thinking".into()))
                    .or_else(|| text_delta.as_ref().map(|_| "Responding".into()));
                if let Some(delta) = reasoning_delta {
                    workspace
                        .chat
                        .push_stream_delta(crate::state::StreamingKind::Thinking, &delta);
                }
                if let Some(delta) = text_delta {
                    workspace
                        .chat
                        .push_stream_delta(crate::state::StreamingKind::Assistant, &delta);
                }
                if tool_call_name.is_some() {
                    workspace.chat.flush_tool_call_preamble();
                }
                if let Some(detail) = activity_detail {
                    set_live_main_activity(&mut workspace.chat, detail.clone());
                    self.set_live_composer_activity(cx, &key, &detail);
                }
            }
            AgentEvent::MessageEnd { message } => {
                let Some(key) = target_key else { return };
                let workspace = self.workspace_state.workspace_mut(key.clone());
                if matches!(
                    message,
                    threadlane_agent::AgentMessage::Assistant {
                        tool_calls: Some(_),
                        ..
                    }
                ) {
                    workspace.chat.flush_tool_call_preamble();
                } else {
                    workspace.chat.flush_streaming();
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                name,
                arguments,
            } => {
                let Some(key) = target_key else { return };
                let activity_detail = format!("Running tool: {name}");
                let workspace = self.workspace_state.workspace_mut(key.clone());
                workspace.chat.push_tool(tool_call_id, name, arguments);
                set_live_main_activity(&mut workspace.chat, activity_detail.clone());
                self.set_live_composer_activity(cx, &key, &activity_detail);
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
            } => {
                let Some(key) = target_key else { return };
                self.workspace_state.workspace_mut(key).chat.update_tool(
                    &tool_call_id,
                    partial_result,
                    None,
                );
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                ..
            } => {
                let Some(key) = target_key else { return };
                let workspace = self.workspace_state.workspace_mut(key.clone());
                workspace.chat.update_tool(
                    &tool_call_id,
                    result.content,
                    Some(if result.is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Done
                    }),
                );
                set_live_main_activity(&mut workspace.chat, "Working");
                self.set_live_composer_activity(cx, &key, "Working");
            }
            AgentEvent::TurnEnd { .. } => {
                if let Some(key) = target_key {
                    self.set_session_status(cx, &key, UiStatus::Working, "Turn completed");
                }
            }
            // AgentEnd closes one agent loop, but CodingAgent may still run
            // hooks or scheduled work. GenerationFinished is the terminal event.
            AgentEvent::AgentEnd { .. } if generation.is_none() => (),
            AgentEvent::AgentEnd { usage } => {
                let Some((key, id)) = generation else { return };
                let accepted = self.session_runtimes.get(&key).is_some_and(|runtime| {
                    accepts_generation_event(
                        runtime.generation.as_ref().map(|generation| generation.id),
                        runtime.terminal_generation_id,
                        id,
                        GenerationEvent::AgentEnd,
                    )
                });
                if !accepted {
                    return;
                }
                if let Some(runtime) = self.session_runtimes.get_mut(&key) {
                    runtime.latest_usage = Some(usage.clone());
                    let agent = runtime.agent.clone();
                    get_runtime().spawn(async move {
                        let mut agent = agent.lock().await;
                        if let Ok(value) = serde_json::to_string(&usage) {
                            let _ = agent.set_fact(CONTEXT_USAGE_FACT, &value);
                        }
                    });
                }
                self.sync_context_window(cx);
                self.workspace_state
                    .workspace_mut(key.clone())
                    .chat
                    .flush_streaming();
                set_live_main_activity(
                    &mut self.workspace_state.workspace_mut(key.clone()).chat,
                    "Finishing",
                );
                self.set_session_status(cx, &key, UiStatus::Working, "Finishing...");
            }
            AgentEvent::AgentError { error } => {
                let Some((key, id)) = generation else {
                    self.push_chat(MsgRole::System, format!("Agent error: {error}"));
                    let status = concise_status(&error);
                    self.set_status(cx, UiStatus::Error, &status);
                    return;
                };
                let accepted = self.session_runtimes.get(&key).is_some_and(|runtime| {
                    accepts_generation_event(
                        runtime.generation.as_ref().map(|generation| generation.id),
                        runtime.terminal_generation_id,
                        id,
                        GenerationEvent::AgentError,
                    )
                });
                if !accepted {
                    return;
                }

                let (restored_draft, restored_attachments) = self
                    .session_runtimes
                    .get_mut(&key)
                    .map(|runtime| {
                        runtime.generation = None;
                        runtime.terminal_generation_id = None;
                        let draft = runtime
                            .submitted_draft
                            .take()
                            .filter(|(draft_id, _)| *draft_id == id)
                            .map(|(_, draft)| draft);
                        let attachments = runtime
                            .submitted_attachments
                            .take()
                            .filter(|(attachment_id, _)| *attachment_id == id)
                            .map(|(_, attachments)| attachments);
                        (draft, attachments)
                    })
                    .unwrap_or_default();
                let is_active = self.workspace_state.is_active(&key);
                let current_draft = if is_active {
                    self.ui
                        .threadlane_command_text_input(cx, ids!(prompt_input))
                        .text_input_ref(cx)
                        .text()
                } else {
                    self.workspace_state
                        .workspace(&key)
                        .map(|workspace| workspace.ui.draft.clone())
                        .unwrap_or_default()
                };
                let draft = if current_draft.trim().is_empty() {
                    restored_draft.unwrap_or_default()
                } else {
                    current_draft
                };
                let workspace = self.workspace_state.workspace_mut(key.clone());
                workspace.chat.flush_streaming();
                clear_live_main_activity(&mut workspace.chat);
                workspace.ui.draft = draft.clone();
                if let Some(attachments) = restored_attachments {
                    workspace.ui.attachments = attachments;
                }
                workspace
                    .chat
                    .push_chat(MsgRole::System, format!("Agent error: {error}"));
                if is_active {
                    self.ui
                        .threadlane_command_text_input(cx, ids!(prompt_input))
                        .text_input_ref(cx)
                        .set_text(cx, &draft);
                    self.refresh_attachment_ui(cx);
                }
                let status = concise_status(&error);
                self.set_session_status(cx, &key, UiStatus::Error, &status);
            }
            AgentEvent::StreamRuleTriggered {
                rule_name,
                matched_text,
                reminder,
                ..
            } => {
                let Some(key) = target_key else { return };
                let workspace = self.workspace_state.workspace_mut(key);
                workspace.chat.flush_streaming();
                workspace.chat.push_chat(
                    MsgRole::System,
                    format!("⚠ Injected stream rule '{rule_name}' after matching '{matched_text}': {reminder}"),
                );
            }
            AgentEvent::PlanUpdated { plan } => {
                let Some(key) = target_key else { return };
                if let Some(runtime) = self.session_runtimes.get_mut(&key) {
                    runtime.plan = plan;
                }
                if self.workspace_state.is_active(&key) {
                    self.sync_task_sidebar(cx);
                }
            }
            event @ (AgentEvent::SubagentQueued { .. }
            | AgentEvent::SubagentStarted { .. }
            | AgentEvent::SubagentFinished { .. }
            | AgentEvent::SubagentRecovery { .. }) => {
                let Some(key) = target_key else { return };
                let health = {
                    let chat = &mut self.workspace_state.workspace_mut(key.clone()).chat;
                    reduce_harness_event(chat, event);
                    session_health(&chat.harness_activities)
                };
                set_session_health(&key.work_dir, &key.session_id, health);
                self.ui.widget(cx, ids!(session_list)).redraw(cx);
            }
            AgentEvent::TurnStart { .. } | AgentEvent::MessageStart { .. } => {}
        }
    }

    pub fn poll_agent_events(&mut self, cx: &mut Cx) {
        let mut events = Vec::new();
        if let Some(rx_arc) = &self.rx {
            if let Ok(rx) = rx_arc.lock() {
                while let Ok(evt) = rx.try_recv() {
                    events.push(evt);
                }
            }
        }
        if events.is_empty() {
            return;
        }

        for evt in events {
            match evt {
                GuiAgentEvent::CommandOutput {
                    generation_id,
                    work_dir,
                    session_id,
                    output,
                } => {
                    let key = SessionKey::new(work_dir, session_id);
                    let is_current_generation =
                        self.session_runtimes.get(&key).is_some_and(|runtime| {
                            accepts_generation_event(
                                runtime.generation.as_ref().map(|generation| generation.id),
                                runtime.terminal_generation_id,
                                generation_id,
                                GenerationEvent::CommandOutput,
                            )
                        });
                    if !is_current_generation {
                        continue;
                    }
                    self.workspace_state
                        .workspace_mut(key)
                        .chat
                        .push_chat(MsgRole::System, output);
                }
                GuiAgentEvent::GenerationFinished {
                    generation_id,
                    work_dir,
                    session_id,
                } => {
                    let key = SessionKey::new(work_dir.clone(), session_id.clone());
                    let is_current = self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.generation.as_ref())
                        .is_some_and(|generation| generation.id == generation_id);
                    if !is_current {
                        continue;
                    }
                    if let Some(runtime) = self.session_runtimes.get_mut(&key) {
                        runtime.generation = None;
                        runtime.terminal_generation_id = None;
                        runtime.submitted_draft = None;
                        runtime.submitted_attachments = None;
                    }
                    let workspace = self.workspace_state.workspace_mut(key.clone());
                    workspace.chat.flush_streaming();
                    clear_live_main_activity(&mut workspace.chat);
                    self.set_session_status(cx, &key, UiStatus::Ready, "Ready");

                    // If a message was pending in the queue popup, dispatch it now.
                    if let Some(text) = self.pending_queue_text.take() {
                        self.pending_queue_attachments.clear();
                        self.ui
                            .widget(cx, ids!(queued_message_preview))
                            .set_visible(cx, false);
                        self.dispatch_input(cx, text, InputOrigin::Composer);
                    }

                    if self.finish_session_tasks(&work_dir, &session_id) {
                        self.sync_task_sidebar(cx);
                    }
                    self.refresh_registered_sessions();
                    if self
                        .active_work_dir()
                        .is_some_and(|active| active == work_dir)
                    {
                        self.request_git_status();
                    }
                }

                GuiAgentEvent::HarnessEvent {
                    generation_id,
                    work_dir,
                    session_id,
                    event,
                } => {
                    let _ = event.id;
                    let key = SessionKey::new(work_dir, session_id);
                    let is_current = self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.generation.as_ref())
                        .is_some_and(|generation| generation.id == generation_id);
                    if !is_current {
                        continue;
                    }
                    if let EventPayload::Streaming(state) = &event.payload {
                        if let Some(stream) = state {
                            let detail = harness_live_streaming_detail(stream);
                            set_live_main_activity(
                                &mut self.workspace_state.workspace_mut(key.clone()).chat,
                                detail,
                            );
                        }
                        self.ui.widget(cx, ids!(chat_list)).redraw(cx);
                        continue;
                    }
                    if let Some(path) = self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.session_file.as_deref())
                    {
                        let live_main = self
                            .workspace_state
                            .workspace(&key)
                            .and_then(|workspace| {
                                workspace
                                    .chat
                                    .harness_activities
                                    .iter()
                                    .find(|activity| {
                                        activity.agent == "main"
                                            && activity.status
                                                == crate::panels::chat::state::HarnessActivityStatus::Working
                                    })
                                    .cloned()
                            });
                        let mut activities = restore_harness_activities(path);
                        if let Some(activity) = live_main {
                            activities.push(activity);
                        }
                        if let EventPayload::Fault(error) =
                            &event.payload
                        {
                            activities.push(HarnessActivity {
                                key: format!("harness-fault-{}", event.id),
                                task: "Harness storage".into(),
                                agent: "main".into(),
                                status: crate::panels::chat::state::HarnessActivityStatus::Faulted,
                                detail: format!("Harness storage fault: {error}"),
                            });
                        }
                        suppress_live_main_recovery(&mut activities, is_current);
                        let health = session_health(&activities);
                        self.workspace_state
                            .workspace_mut(key.clone())
                            .chat
                            .harness_activities = activities;
                        set_session_health(&key.work_dir, &key.session_id, health);
                        self.ui.widget(cx, ids!(session_list)).redraw(cx);
                    }
                }

                GuiAgentEvent::HarnessSnapshot {
                    generation_id,
                    work_dir,
                    session_id,
                    snapshot,
                } => {
                    let key = SessionKey::new(work_dir, session_id);
                    let is_current = self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.generation.as_ref())
                        .is_some_and(|generation| generation.id == generation_id);
                    if !is_current {
                        continue;
                    }
                    if self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.session_file.as_deref())
                        .is_none()
                    {
                        continue;
                    }
                    let mut activities = harness_activities_from_snapshot(&snapshot);
                    suppress_live_main_recovery(&mut activities, is_current);
                    let health = session_health(&activities);
                    self.workspace_state
                        .workspace_mut(key.clone())
                        .chat
                        .harness_activities = activities;
                    set_session_health(&key.work_dir, &key.session_id, health);
                    self.ui.widget(cx, ids!(session_list)).redraw(cx);
                }

                GuiAgentEvent::HarnessResumeFinished {
                    work_dir,
                    session_id,
                    result,
                } => {
                    let key = SessionKey::new(work_dir, session_id);
                    if let Some(runtime) = self.session_runtimes.get(&key) {
                        if let Some(path) = runtime.session_file.as_deref() {
                            let activities = restore_harness_activities(path);
                            self.workspace_state
                                .workspace_mut(key.clone())
                                .chat
                                .harness_activities = activities;
                        }
                    }
                    if let Err(error) = result {
                        self.push_chat_to(
                            key.clone(),
                            MsgRole::System,
                            format!("Harness resume failed: {error}"),
                        );
                    }
                    self.ui.widget(cx, ids!(chat_list)).redraw(cx);
                    self.set_session_status(cx, &key, UiStatus::Ready, "Ready");
                }

                GuiAgentEvent::SessionTitleGenerated => {
                    self.refresh_registered_sessions();
                    self.ui.widget(cx, ids!(session_list)).redraw(cx);
                }
                GuiAgentEvent::AvailableModelsLoaded(models) => {
                    let selected_model = self
                        .ui
                        .icon_drop_down(cx, ids!(model_drop))
                        .selected_label();
                    self.set_model_dropup_options(
                        cx,
                        include_connected_provider_models(models),
                        &selected_model,
                    );
                }
                GuiAgentEvent::ProjectFolderPicked(result) => {
                    self.apply_project_folder_result(cx, result);
                }
                GuiAgentEvent::ExtensionFilePicked {
                    path: Some(path),
                    scope,
                } => {
                    self.install_extension(cx, path, scope);
                }
                GuiAgentEvent::ExtensionFilePicked { path: None, .. } => {}
                GuiAgentEvent::ExtensionReloadCompleted {
                    reload_id,
                    reloaded,
                    failures,
                } => {
                    if reload_id != self.next_extension_reload_id {
                        continue;
                    }
                    self.refresh_capability_state(cx);
                    if let Some(work_dir) = self.active_work_dir().map(Path::to_path_buf) {
                        self.refresh_project_capabilities(cx, &work_dir);
                    } else {
                        self.commands = builtin_commands();
                    }
                    self.set_capability_status(cx, &extension_reload_status(reloaded, &failures));
                }
                GuiAgentEvent::DeviceCodePrompt { user_code, url } => {
                    if let Some(key) = self.auth_workspace.clone() {
                        self.push_chat_to(
                            key.clone(),
                            MsgRole::System,
                            format!(
                                "Sign in: open {url} in your browser and enter code {user_code} \
                                 (waiting for authorization...)"
                            ),
                        );
                        let _ = robius_open::Uri::new(&url).open();
                        if self.workspace_state.is_active(&key) {
                            self.apply_status_ui(
                                cx,
                                UiStatus::Working,
                                &format!("Enter code {user_code}"),
                            );
                        }
                    }
                }
                GuiAgentEvent::DeviceLoginSuccess => {
                    let mut key_opt = None;
                    let mut acc_opt = None;
                    if let Some(creds) = auth::load_credentials() {
                        self.ui
                            .text_input(cx, ids!(api_key_input))
                            .set_text(cx, &creds.access_token);
                        key_opt = Some(creds.access_token.clone());
                        acc_opt = creds.account_id;
                    }
                    if let Some(key) = self.auth_workspace.take() {
                        self.push_chat_to(
                            key.clone(),
                            MsgRole::System,
                            "Successfully authenticated with ChatGPT.",
                        );
                        if self.workspace_state.is_active(&key) {
                            self.restore_active_status(cx);
                        }
                    }
                    self.ui.widget(cx, ids!(auth_row)).set_visible(cx, false);

                    if let Some(key) = key_opt {
                        self.spawn_model_fetch(key, acc_opt);
                    }
                }
                GuiAgentEvent::DeviceLoginError(error) => {
                    if let Some(key) = self.auth_workspace.take() {
                        self.push_chat_to(
                            key.clone(),
                            MsgRole::System,
                            format!("Authentication error: {error}"),
                        );
                        if self.workspace_state.is_active(&key) {
                            self.apply_status_ui(cx, UiStatus::Error, &concise_status(&error));
                        }
                    }
                }
                GuiAgentEvent::AntigravityLoginSuccess { email } => {
                    let msg = match email {
                        Some(e) => {
                            format!("✓ Successfully authenticated with Google Antigravity ({e}).")
                        }
                        None => "✓ Successfully authenticated with Google Antigravity.".to_string(),
                    };
                    self.push_chat(MsgRole::System, msg);
                    self.ui.widget(cx, ids!(auth_row)).set_visible(cx, false);
                    self.set_status(cx, UiStatus::Ready, "Ready");
                    let selected_model = self
                        .ui
                        .icon_drop_down(cx, ids!(model_drop))
                        .selected_label();
                    let mut models = self.available_models.clone();
                    append_antigravity_models(&mut models);
                    self.set_model_dropup_options(cx, models, &selected_model);
                    cx.redraw_all();
                }
                GuiAgentEvent::AntigravityLoginError(error) => {
                    self.push_chat(
                        MsgRole::System,
                        format!("❌ Google Antigravity login error: {error}"),
                    );
                    cx.redraw_all();
                }
                GuiAgentEvent::AntigravityDoctorReport(report) => {
                    self.push_chat(MsgRole::System, report);
                    cx.redraw_all();
                }
                GuiAgentEvent::GitStatusLoaded {
                    request_id,
                    work_dir,
                    result,
                } => {
                    if request_id != self.next_git_request_id {
                        continue;
                    }
                    self.git_status_pending = false;
                    match result {
                        Ok(status) => {
                            self.git_status.insert(work_dir, status);
                        }
                        Err(_) => {
                            // Never leave actionable controls backed by a
                            // status snapshot that Git can no longer verify.
                            self.git_status.remove(&work_dir);
                            self.git_feedback = None;
                        }
                    }
                    self.sync_git_branch_picker(cx);
                    self.sync_right_sidebar(cx);
                }
                GuiAgentEvent::GitOperationFinished {
                    request_id,
                    work_dir,
                    operation,
                    result,
                } => {
                    if request_id != self.git_operation_request_id
                        || self
                            .active_work_dir()
                            .is_none_or(|active| active != work_dir)
                    {
                        continue;
                    }
                    self.git_operation_pending = false;
                    match result {
                        Ok(()) => {
                            if operation == "commit" {
                                self.ui
                                    .text_input(cx, ids!(git_commit_message))
                                    .set_text(cx, "");
                            }
                            if operation.starts_with("create worktree ") {
                                if let (Some(key), Some(path)) = (
                                    self.workspace_state.active_key().cloned(),
                                    self.pending_worktree_path.take(),
                                ) {
                                    self.checkout_targets.insert(key, path);
                                }
                                self.set_worktree_prompt_visible(cx, false);
                                self.rebind_active_runtime_to_target(cx);
                                self.sync_git_branch_picker(cx);
                            }
                            let message = format!("Git {operation} completed.");
                            self.git_feedback = Some((true, message));
                        }
                        Err(error) => {
                            if operation.starts_with("create worktree ") {
                                self.pending_worktree_path = None;
                            }
                            let message = format!("Git {operation} failed: {error}");
                            self.git_feedback = Some((false, message));
                        }
                    }
                    self.sync_right_sidebar(cx);
                    self.git_status.remove(&work_dir);
                    self.request_git_status();
                }

                GuiAgentEvent::GitDiffLoaded {
                    request_id,
                    path,
                    result,
                } => {
                    self.git_diff_pending = false;
                    if request_id != self.git_diff_request_id {
                        continue;
                    }
                    match result {
                        Ok(diff) => {
                            self.git_diff_open = true;
                            self.ui.label(cx, ids!(git_diff_path)).set_text(cx, &path);
                            if let Some(mut diff_view) = self
                                .ui
                                .widget(cx, ids!(git_diff_text))
                                .borrow_mut::<GitDiffView>()
                            {
                                diff_view.set_text(cx, &diff);
                            }
                            self.sync_right_sidebar(cx);
                        }
                        Err(error) => {
                            self.git_diff_open = false;
                            let message = format!("Could not load diff for `{path}`: {error}");
                            self.git_feedback = Some((false, message));
                            self.sync_right_sidebar(cx);
                        }
                    }
                }
                GuiAgentEvent::GitCommitMessageGenerated {
                    request_id,
                    work_dir,
                    result,
                } => {
                    if request_id != self.git_commit_message_request_id
                        || self
                            .active_work_dir()
                            .is_none_or(|active| active != work_dir)
                    {
                        continue;
                    }
                    self.git_commit_message_abort = None;
                    self.git_commit_message_pending = false;
                    match result {
                        Ok(raw) => {
                            let message = normalize_generated_commit_message(&raw);
                            if message.is_empty() {
                                self.git_feedback = Some((
                                    false,
                                    "The model returned an empty commit message.".to_owned(),
                                ));
                            } else if self
                                .ui
                                .text_input(cx, ids!(git_commit_message))
                                .text()
                                .trim()
                                .is_empty()
                            {
                                self.ui
                                    .text_input(cx, ids!(git_commit_message))
                                    .set_text(cx, &message);
                                self.git_feedback =
                                    Some((true, "Commit message generated.".to_owned()));
                            } else {
                                self.git_feedback = Some((
                                    true,
                                    "Commit message kept; generation finished.".to_owned(),
                                ));
                            }
                        }
                        Err(error) => {
                            eprintln!("[commit_message_gen] Error: {error}");
                            self.git_feedback = Some((
                                false,
                                format!("Could not generate commit message: {error}"),
                            ));
                        }
                    }
                    self.sync_right_sidebar(cx);
                }
                GuiAgentEvent::McpRefreshCompleted(records) => {
                    self.capability_state.refresh_mcp_records(records);
                    if let Some(mut modal) = self
                        .ui
                        .widget(cx, ids!(providers_modal))
                        .borrow_mut::<ProviderSettingsModal>()
                    {
                        modal.set_mcp_rows(cx, self.capability_state.mcp_servers.clone());
                        modal.set_mcp_status(cx, "");
                    }
                }
                GuiAgentEvent::AcpSessionStarted {
                    work_dir,
                    session_id,
                    chat,
                } => {
                    let key = SessionKey::new(work_dir, session_id);
                    if let Some(runtime) = self.session_runtimes.get_mut(&key) {
                        runtime.acp = Some(chat);
                    }
                }
                GuiAgentEvent::AcpRefreshCompleted(records) => {
                    self.capability_state.refresh_acp_records(records);
                    if let Some(mut modal) = self
                        .ui
                        .widget(cx, ids!(providers_modal))
                        .borrow_mut::<ProviderSettingsModal>()
                    {
                        modal.set_acp_rows(cx, self.capability_state.acp_agents.clone());
                    }
                }
                GuiAgentEvent::BackgroundTask(event) => {
                    let (task_id, project_id, agent_event) = event.into_parts();
                    let project_work_dir = self
                        .supervisor_projects
                        .iter()
                        .find(|(_, pid)| *pid == &project_id)
                        .map(|(work_dir, _)| work_dir.clone());
                    if let (Some(work_dir), Some(supervisor)) =
                        (project_work_dir, &self.supervisor)
                    {
                        if let Some(task) = supervisor.get_task(&task_id) {
                            let activity = background_task_harness_activity(
                                &task_id, &task, &agent_event,
                            );
                            if let Some(activity) = activity {
                                let keys: Vec<SessionKey> = self
                                    .workspace_state
                                    .keys_for_project(&work_dir)
                                    .cloned()
                                    .collect();
                                for key in keys {
                                    crate::panels::chat::state::reduce_harness_activity(
                                        &mut self
                                            .workspace_state
                                            .workspace_mut(key)
                                            .chat
                                            .harness_activities,
                                        activity.clone(),
                                    );
                                }
                                self.ui.widget(cx, ids!(chat_list)).redraw(cx);
                            }
                        }
                    }
                    self.refresh_registered_sessions();
                    self.sync_task_sidebar(cx);
                }
                GuiAgentEvent::GenerationAgent {
                    generation_id,
                    work_dir,
                    session_id,
                    event: agent_event,
                } => {
                    let key = SessionKey::new(work_dir.clone(), session_id.clone());
                    let is_current = self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.generation.as_ref())
                        .is_some_and(|generation| generation.id == generation_id);
                    if !is_current {
                        continue;
                    }
                    let session_file = self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.session_file.as_deref());
                    if self.observe_session_task_event(
                        &work_dir,
                        &session_id,
                        session_file,
                        &agent_event,
                    ) {
                        self.sync_task_sidebar(cx);
                    }
                    self.handle_agent_event(cx, agent_event, Some((key, generation_id)))
                }
            }
        }

        self.schedule_chat_redraw(cx);
    }
}

#[cfg(test)]
mod workspace_header_tests {
    use super::{
        aggregate_extension_reload_results, append_antigravity_models, clear_composer_for_dispatch,
        compact_workspace_path, extension_reload_matches, extension_reload_status,
        left_sidebar_splitter_align, model_credential_error, normalize_generated_commit_message,
        ordered_model_options, project_name, reduce_harness_event, restore_harness_activities,
        session_reload_count, suppress_live_main_recovery, task_sidebar_items,
        truncate_terminal_output, InputOrigin, ANTIGRAVITY_MODELS, LEFT_SIDEBAR_WIDTH,
        MAX_TERMINAL_OUTPUT,
    };
    use crate::panels::chat::state::HarnessActivityStatus;
    use crate::workspace::WorkspaceUiState;
    use makepad_widgets::SplitterAlign;
    use std::path::{Path, PathBuf};
    use threadlane_agent::harness::{Entry, JsonlStore, OperationIntent, Record, SessionStore};
    use threadlane_agent::AgentMessage;
    use threadlane_agent::{AgentEvent, ImageAttachment, SubagentRecoveryStatus};
    use threadlane_coding_agent::{ExtensionScope, TaskKind, TaskRecord, TaskStatus};

    #[test]
    fn harness_lane_activity_includes_live_tool_queue_and_usage() {
        let mut lane = threadlane_agent::harness::LaneState::default();
        lane.tools.push(threadlane_agent::harness::ToolState {
            run_id: "run-1".into(),
            assistant_entry_id: "assistant-1".into(),
            tool_index: 0,
            tool_call_id: "call-1".into(),
            tool_name: "shell".into(),
            result_entry_id: "result-1".into(),
            replay: threadlane_agent::harness::ToolReplaySafety::Safe,
            completed: false,
            terminate: false,
        });
        lane.usage.total_tokens = 1_234;
        lane.queued.push(threadlane_agent::harness::QueuedEntry {
            id: "queue-1".into(),
            run_id: Some("run-1".into()),
            queue: threadlane_agent::harness::QueueKind::FollowUp,
            priority: None,
            target: threadlane_agent::harness::ProvisionedEntry {
                id: "entry-1".into(),
                parent_id: None,
                message: AgentMessage::User {
                    content: "next".into(),
                },
            },
        });

        assert_eq!(
            super::harness_lane_activity(&lane, None),
            "Running tool: shell · replay-safe · queued: follow-up 1 · 1.2k tokens"
        );
    }

    #[test]
    fn live_main_activity_is_visible_while_foreground_generation_runs() {
        let activity = super::live_main_activity("Thinking");
        assert_eq!(activity.key, "main-live");
        assert_eq!(activity.agent, "main");
        assert_eq!(activity.status, HarnessActivityStatus::Working);
        assert_eq!(activity.detail, "Thinking");

        let mut chat = crate::panels::chat::state::ChatData::default();
        super::set_live_main_activity(&mut chat, "Thinking");
        assert_eq!(chat.revision, 1);
    }

    #[test]
    fn generated_commit_messages_are_normalized_to_one_subject_line() {
        assert_eq!(
            normalize_generated_commit_message("```\nCommit: Fix branch picker\n```"),
            "Fix branch picker"
        );
        assert_eq!(
            normalize_generated_commit_message("feat: add generated commit messages\nDetails"),
            "feat: add generated commit messages"
        );
    }

    #[test]
    fn restore_harness_activities_surfaces_a_suspended_v2_main_run() {
        let session_file = std::env::temp_dir().join(format!(
            "threadlane-harness-v2-restore-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&session_file);
        let _ = std::fs::remove_file(session_file.with_extension("harness.jsonl"));
        std::fs::File::create(&session_file).unwrap();
        let mut store = JsonlStore::open(&session_file).unwrap();
        store
            .append_entry(Entry {
                id: "node-1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::user("Resume this", vec![]),
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-v2".into(),
                seq: 2,
                lane: "main".into(),
                timestamp: 2,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();

        let activities = restore_harness_activities(&session_file);
        assert!(activities.iter().any(|activity| {
            activity.key == "main-run-v2"
                && activity.task == "Resume this"
                && activity.status == HarnessActivityStatus::Recovering
        }));
        let _ = std::fs::remove_file(&session_file);
        let _ = std::fs::remove_file(session_file.with_extension("harness.jsonl"));
    }

    #[test]
    fn live_foreground_runs_do_not_look_like_recovery() {
        let mut activities = vec![crate::panels::chat::state::HarnessActivity {
            key: "main-run".into(),
            task: "Inspect the repo".into(),
            agent: "main".into(),
            status: HarnessActivityStatus::Recovering,
            detail: "Suspended operation".into(),
        }];
        suppress_live_main_recovery(&mut activities, true);
        assert!(activities.is_empty());
        assert_eq!(
            crate::panels::sessions::state::session_health(&activities),
            crate::panels::sessions::state::SessionHealth::Healthy
        );
    }

    #[test]
    fn restore_harness_activities_surfaces_a_suspended_v2_subagent_run() {
        let session_file = std::env::temp_dir().join(format!(
            "threadlane-harness-v2-subagent-restore-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&session_file);
        let _ = std::fs::remove_file(session_file.with_extension("harness.jsonl"));
        std::fs::File::create(&session_file).unwrap();
        let mut store = JsonlStore::open(&session_file).unwrap();
        store
            .append_entry(Entry {
                id: "subagent-task".into(),
                parent_id: None,
                lane: "subagent-1@1".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::user("Inspect this", vec![]),
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "subagent-run-1".into(),
                seq: 2,
                lane: "subagent-1@1".into(),
                timestamp: 2,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();

        let activities = restore_harness_activities(&session_file);
        assert!(activities.iter().any(|activity| {
            activity.key == "subagent-run-1"
                && activity.task == "Inspect this"
                && activity.status == HarnessActivityStatus::Recovering
        }));
        let _ = std::fs::remove_file(&session_file);
        let _ = std::fs::remove_file(session_file.with_extension("harness.jsonl"));
    }

    #[test]
    fn task_sidebar_items_preserve_session_navigation_and_cancel_policy() {
        let session_file = PathBuf::from("/project/.threadlane/sessions/chat.jsonl");
        let items = task_sidebar_items(
            vec![TaskRecord {
                id: "task-1".into(),
                project_id: "project-1".into(),
                session_id: "chat".into(),
                session_file: Some(session_file.clone()),
                parent_task_id: None,
                kind: TaskKind::Background,
                agent: "task".into(),
                summary: "Inspect the workspace".into(),
                current_activity: Some("read_file".into()),
                status: TaskStatus::Running,
                started_at_ms: 1,
                finished_at_ms: None,
            }],
            |_| Some("Architecture review".into()),
        );

        assert_eq!(items[0].session_label, "Architecture review");
        assert_eq!(items[0].session_file.as_ref(), Some(&session_file));
        assert!(items[0].cancellable);
    }

    #[test]
    fn partial_journal_start_and_recovery_share_one_harness_activity() {
        let mut chat = crate::panels::chat::ChatData::default();
        for event in [
            AgentEvent::SubagentQueued {
                run_id: 7,
                task_index: 0,
                agent: "scout".into(),
                task: "Inspect the repository".into(),
            },
            AgentEvent::SubagentFinished {
                run_id: 7,
                task_index: 0,
                journal_run_id: "subagent-run-41".into(),
                succeeded: false,
                error: Some("Failed to append subagent lane journal".into()),
            },
            AgentEvent::SubagentRecovery {
                run_id: "subagent-run-41".into(),
                status: SubagentRecoveryStatus::Recovered,
                detail: Some("Recovered prior work".into()),
            },
        ] {
            reduce_harness_event(&mut chat, event);
        }

        assert_eq!(chat.harness_activities.len(), 1);
        assert_eq!(chat.harness_activities[0].key, "subagent-run-41");
        assert_eq!(
            chat.harness_activities[0].status,
            HarnessActivityStatus::Recovered
        );
    }

    #[test]
    fn extension_reload_results_preserve_successes_and_labeled_failures() {
        let outcome = aggregate_extension_reload_results([
            ("session alpha".to_owned(), Ok(1)),
            ("session beta".to_owned(), Err("invalid module".to_owned())),
            ("background tasks".to_owned(), Ok(2)),
        ]);

        assert_eq!(outcome.reloaded, 3);
        assert_eq!(
            outcome.failures,
            ["session beta: invalid module".to_owned()]
        );
    }

    #[test]
    fn session_reload_counts_one_session_not_loaded_extensions() {
        assert_eq!(session_reload_count(Ok(7)), Ok(1));
        assert_eq!(
            session_reload_count(Err("invalid module".to_owned())),
            Err("invalid module".to_owned())
        );
    }

    #[test]
    fn extension_reload_status_surfaces_success() {
        assert_eq!(
            extension_reload_status(2, &[]),
            "Reloaded extensions in 2 live sessions."
        );
    }

    #[test]
    fn extension_reload_status_surfaces_failures() {
        let failures = vec!["session beta: invalid module".to_owned()];

        assert_eq!(
            extension_reload_status(1, &failures),
            "Reloaded 1 live session; failed for session beta: invalid module"
        );
    }

    #[test]
    fn extension_reload_scope_selects_matching_session_runtimes() {
        let changed_project = Path::new("/projects/alpha");

        assert!(extension_reload_matches(
            ExtensionScope::Global,
            changed_project,
            Path::new("/projects/alpha"),
        ));
        assert!(extension_reload_matches(
            ExtensionScope::Global,
            changed_project,
            Path::new("/projects/beta"),
        ));
        assert!(extension_reload_matches(
            ExtensionScope::Project,
            changed_project,
            Path::new("/projects/alpha"),
        ));
        assert!(!extension_reload_matches(
            ExtensionScope::Project,
            changed_project,
            Path::new("/projects/beta"),
        ));
    }

    #[test]
    fn internal_model_switch_preserves_composer_draft_and_attachments() {
        let mut composer = WorkspaceUiState {
            draft: "keep this draft".to_string(),
            attachments: vec![ImageAttachment {
                display_name: "diagram.png".to_string(),
                data_url: "data:image/png;base64,AAAA".to_string(),
            }],
        };

        clear_composer_for_dispatch(InputOrigin::Internal, &mut composer);

        assert_eq!(composer.draft, "keep this draft");
        assert_eq!(composer.attachments.len(), 1);
        assert_eq!(composer.attachments[0].display_name, "diagram.png");

        clear_composer_for_dispatch(InputOrigin::Composer, &mut composer);

        assert!(composer.draft.is_empty());
        assert!(composer.attachments.is_empty());
    }

    #[test]
    fn provider_credentials_follow_the_selected_model() {
        assert_eq!(
            model_credential_error("antigravity/gemini-3.6-flash", false, true, false),
            None
        );
        assert_eq!(
            model_credential_error("antigravity/gemini-3.6-flash", true, false, false),
            Some("Sign in with Google Antigravity before using this model.")
        );
        assert_eq!(
            model_credential_error("gpt-5.6-luna", false, true, false),
            Some("Please provide an OpenAI API key or click 'Login ChatGPT' to authenticate.")
        );
    }

    #[test]
    fn antigravity_models_merge_without_duplicates() {
        let mut models = vec![
            "gpt-5.6-luna".to_string(),
            ANTIGRAVITY_MODELS[0].to_string(),
        ];

        append_antigravity_models(&mut models);

        assert_eq!(
            models
                .iter()
                .filter(|model| model.as_str() == ANTIGRAVITY_MODELS[0])
                .count(),
            1
        );
        assert!(ANTIGRAVITY_MODELS
            .iter()
            .all(|model| models.iter().any(|candidate| candidate == model)));
    }

    #[test]
    fn model_order_stays_grouped_across_provider_switches() {
        let models = vec![
            "gpt-a".to_string(),
            "antigravity/gemini-a".to_string(),
            "gpt-b".to_string(),
            "antigravity/gemini-b".to_string(),
        ];

        let (canonical, google_selected) =
            ordered_model_options(models, "antigravity/gemini-a").unwrap();
        assert_eq!(
            canonical,
            [
                "gpt-a",
                "gpt-b",
                "antigravity/gemini-a",
                "antigravity/gemini-b"
            ]
        );
        assert_eq!(
            google_selected,
            [
                "gpt-a",
                "gpt-b",
                "antigravity/gemini-b",
                "antigravity/gemini-a"
            ]
        );

        let (canonical, openai_selected) = ordered_model_options(canonical, "gpt-a").unwrap();
        assert_eq!(
            openai_selected,
            [
                "gpt-b",
                "antigravity/gemini-a",
                "antigravity/gemini-b",
                "gpt-a"
            ]
        );

        let (_, google_selected_again) =
            ordered_model_options(canonical, "antigravity/gemini-b").unwrap();
        assert_eq!(
            google_selected_again,
            [
                "gpt-a",
                "gpt-b",
                "antigravity/gemini-a",
                "antigravity/gemini-b"
            ]
        );
    }

    #[test]
    fn persisted_model_missing_from_provider_results_remains_selected() {
        let (canonical, display) =
            ordered_model_options(vec!["gpt-a".into()], "antigravity/retired-model").unwrap();

        assert_eq!(canonical, ["gpt-a", "antigravity/retired-model"]);
        assert_eq!(display.last().unwrap(), "antigravity/retired-model");
    }

    #[test]
    fn workspace_header_uses_final_directory_as_project_name() {
        assert_eq!(
            project_name(Path::new("/Users/alex/code/threadlane")),
            "threadlane"
        );
    }

    #[test]
    fn left_sidebar_splitter_alignment_tracks_visibility() {
        assert!(matches!(
            left_sidebar_splitter_align(true),
            SplitterAlign::FromA(width) if width == LEFT_SIDEBAR_WIDTH
        ));
        assert!(matches!(
            left_sidebar_splitter_align(false),
            SplitterAlign::FromA(width) if width == 0.0
        ));
    }

    #[test]
    fn terminal_output_truncation_handles_a_multibyte_cutoff() {
        let mut output = "a".repeat(MAX_TERMINAL_OUTPUT - 1);
        output.push('é');
        output.push_str(&"b".repeat(MAX_TERMINAL_OUTPUT - 1));

        truncate_terminal_output(&mut output);

        assert!(output.is_char_boundary(0));
        assert!(output.len() <= MAX_TERMINAL_OUTPUT);
    }

    #[test]
    fn workspace_header_uses_display_path_when_project_has_no_final_directory() {
        assert_eq!(project_name(Path::new("/")), "/");
    }

    #[test]
    fn workspace_header_shortens_paths_below_home() {
        assert_eq!(
            compact_workspace_path(
                Path::new("/Users/alex/Documents/threadlane"),
                Some(Path::new("/Users/alex")),
            ),
            "~/Documents/threadlane"
        );
    }

    #[test]
    fn workspace_header_preserves_home_path_when_nothing_would_be_omitted() {
        assert_eq!(
            compact_workspace_path(
                Path::new("/Users/alex/Documents/exploration/threadlane"),
                Some(Path::new("/Users/alex")),
            ),
            "~/Documents/exploration/threadlane"
        );
    }

    #[test]
    fn workspace_header_compacts_the_middle_of_long_paths() {
        assert_eq!(
            compact_workspace_path(
                Path::new("/Users/alex/Documents/code/client/exploration/threadlane"),
                Some(Path::new("/Users/alex")),
            ),
            "~/Documents/…/exploration/threadlane"
        );
    }

    #[test]
    fn workspace_header_preserves_short_absolute_paths() {
        assert_eq!(
            compact_workspace_path(Path::new("/work/threadlane"), None),
            "/work/threadlane"
        );
    }

    #[test]
    fn workspace_header_does_not_expand_relative_paths_to_home() {
        assert_eq!(
            compact_workspace_path(Path::new("home/project"), Some(Path::new("home"))),
            "home/project"
        );
    }

    #[test]
    fn workspace_header_preserves_root() {
        assert_eq!(compact_workspace_path(Path::new("/"), None), "/");
    }
}
