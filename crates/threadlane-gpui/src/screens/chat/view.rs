use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use std::time::Duration;

use base64::Engine as _;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants, Toggle, ToggleVariants};
use gpui_component::collapsible::Collapsible;
use gpui_component::hover_card::HoverCard;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::popover::Popover;
use gpui_component::progress::ProgressCircle;
use gpui_component::scroll::ScrollableElement;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::theme::ActiveTheme;
use gpui_component::{Disableable, Icon, IconName, Selectable, Sizable, WindowExt};

use crate::app::{actions::AppAction, controller};
use crate::screens::editor::EditorView;
use crate::state::{
    AppState, ChatMessageInfo, ChatStreamEvent, MessageRole, SubagentActivityInfo,
    SubagentActivityStatus, ToolActivityInfo, TrajectoryEntry, WorkMode,
};

fn editor_target_matches_active_work_dir(target: &Path, active: Option<&Path>) -> bool {
    active == Some(target)
}

#[derive(Clone, Debug)]
struct ContextMeterContext {
    current_tokens: u64,
    context_limit: u64,
    context_limit_is_estimate: bool,
    effective_model: String,
    last_compaction_seq: Option<u64>,
    provisional: bool,
    estimating: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct ContextMeterMetrics {
    billed_input_tokens: u64,
    output_tokens: u64,
    cache_hit_percent: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
struct ContextMeterViewModel {
    percent: Option<f64>,
    bar_percent: f64,
    current_label: String,
    detail_label: String,
    total_processed_label: String,
    cache_hit_label: Option<String>,
    effective_model: Option<String>,
    last_compaction_seq: Option<u64>,
    provisional: bool,
}

#[derive(IntoElement)]
struct ContextMeterTrigger {
    toggle: Toggle,
    selected: bool,
}

impl Selectable for ContextMeterTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for ContextMeterTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.toggle.checked(self.selected)
    }
}

#[derive(IntoElement)]
struct SubagentPopoverTrigger {
    toggle: Toggle,
    selected: bool,
}

impl Selectable for SubagentPopoverTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for SubagentPopoverTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.toggle.checked(self.selected)
    }
}

fn format_meter_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn context_meter_view_model(
    context: Option<&ContextMeterContext>,
    metrics: &ContextMeterMetrics,
    reports_usage: bool,
) -> ContextMeterViewModel {
    let total_processed = metrics
        .billed_input_tokens
        .saturating_add(metrics.output_tokens);
    let cache_hit_label = metrics.cache_hit_percent.map(|value| format!("{value}%"));

    // An external ACP agent runs its own loop and reports no token accounting,
    // so there is no context window to measure. Saying "Estimating…" would
    // promise a number that never arrives.
    if !reports_usage {
        return ContextMeterViewModel {
            percent: None,
            bar_percent: 0.0,
            current_label: "Not reported".into(),
            detail_label: "Context usage is not reported by this agent".into(),
            total_processed_label: format_meter_tokens(total_processed),
            cache_hit_label,
            effective_model: None,
            last_compaction_seq: None,
            provisional: false,
        };
    }

    let Some(context) = context else {
        return ContextMeterViewModel {
            percent: None,
            bar_percent: 0.0,
            current_label: "Estimating…".into(),
            detail_label: "Context usage details, estimating usage".into(),
            total_processed_label: format_meter_tokens(total_processed),
            cache_hit_label,
            effective_model: None,
            last_compaction_seq: None,
            provisional: false,
        };
    };

    let unknown = context.estimating || context.context_limit == 0;
    let percent =
        (!unknown).then(|| context.current_tokens as f64 / context.context_limit as f64 * 100.0);
    let limit_prefix = if context.context_limit_is_estimate {
        "~"
    } else {
        ""
    };
    let current_label = if unknown {
        "Estimating…".into()
    } else {
        format!(
            "{} / {limit_prefix}{}",
            format_meter_tokens(context.current_tokens),
            format_meter_tokens(context.context_limit)
        )
    };
    let detail_label = percent.map_or_else(
        || "Context usage details, estimating usage".into(),
        |percent| format!("Context usage details, {percent:.0}% used"),
    );
    ContextMeterViewModel {
        percent,
        bar_percent: percent.unwrap_or_default().clamp(0.0, 100.0),
        current_label,
        detail_label,
        total_processed_label: format_meter_tokens(total_processed),
        cache_hit_label,
        effective_model: (!context.effective_model.is_empty())
            .then(|| context.effective_model.clone()),
        last_compaction_seq: context.last_compaction_seq,
        provisional: context.provisional,
    }
}
use threadlane_session::commands::{available_slash_commands, SlashCommandInfo};
use threadlane_session::{ImageAttachment, PlanItemStatus, ReasoningEffort, SessionPlan};

actions!(
    threadlane_composer,
    [
        PasteClipboard,
        CompleteSlashCommand,
        SelectPreviousSlashCommand,
        SelectNextSlashCommand,
        DismissSlashCommand,
    ]
);

const INPUT_KEY_CONTEXT: &str = "Input";
const SLASH_COMMAND_KEY_CONTEXT: &str = "SlashCommandMenu";
const SLASH_COMMAND_BINDING_CONTEXT: &str = "SlashCommandMenu > Input";

const CHAT_CONTENT_MAX_WIDTH: f32 = 1040.0;
const USER_BUBBLE_MAX_WIDTH: f32 = 680.0;

/// Context-window usage thresholds where the meter shifts to warning/danger colors.
const CONTEXT_METER_WARN_PCT: f64 = 80.0;
const CONTEXT_METER_DANGER_PCT: f64 = 95.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CentralTab {
    #[default]
    Chat,
    Trajectory,
    Editor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum TrajectoryMode {
    #[default]
    Execution,
    Requests,
    ModelContext,
    DurableEvents,
    Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum TrajectoryInspectorTab {
    #[default]
    Overview,
    Preview,
    Raw,
    Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MarkdownUpdate<'a> {
    Unchanged,
    Append(&'a str),
    Replace,
}

fn classify_markdown_update<'a>(current: &str, next: &'a str) -> MarkdownUpdate<'a> {
    if current == next {
        MarkdownUpdate::Unchanged
    } else if let Some(suffix) = next.strip_prefix(current) {
        MarkdownUpdate::Append(suffix)
    } else {
        MarkdownUpdate::Replace
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ChatLinkTarget {
    Web,
    ProjectFile(String),
    Rejected,
}

fn classify_chat_link(link: &str) -> ChatLinkTarget {
    if link.starts_with("http://") || link.starts_with("https://") {
        return ChatLinkTarget::Web;
    }

    let mut normalized = PathBuf::new();
    for component in Path::new(link).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir if normalized.pop() => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return ChatLinkTarget::Rejected;
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        ChatLinkTarget::Rejected
    } else {
        ChatLinkTarget::ProjectFile(normalized.to_string_lossy().into_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownSegment {
    Markdown(String),
    CodeBlock {
        language: String,
        header_path: Option<String>,
        code: String,
    },
}

fn is_terminal_runnable_language(lang: &str) -> bool {
    matches!(
        lang.to_lowercase().as_str(),
        "bash" | "sh" | "zsh" | "shell" | "terminal" | "console" | "cmd" | "powershell"
    )
}

fn active_shell_supports_language(lang: &str) -> bool {
    let shell = std::env::var("SHELL")
        .ok()
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_ascii_lowercase)
        })
        .unwrap_or_else(|| "sh".into());
    match lang.trim().to_ascii_lowercase().as_str() {
        "bash" => shell == "bash",
        "zsh" => shell == "zsh",
        "sh" => matches!(shell.as_str(), "sh" | "bash" | "zsh"),
        "cmd" => matches!(shell.as_str(), "cmd" | "cmd.exe"),
        "powershell" => matches!(shell.as_str(), "pwsh" | "powershell"),
        "shell" | "terminal" | "console" => true,
        _ => false,
    }
}

fn normalize_terminal_command(command: &str) -> String {
    command
        .lines()
        .map(|line| {
            line.strip_prefix("$ ")
                .or_else(|| line.strip_prefix(">>> "))
                .or_else(|| line.strip_prefix("> "))
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn is_fence_line(raw: &str, idx: usize) -> bool {
    let line_start = raw[..idx].rfind('\n').map_or(0, |p| p + 1);
    raw[line_start..idx].len() <= 3 && raw[line_start..idx].bytes().all(|b| b == b' ')
}

fn parse_code_block_header(header: &str, code: &str) -> (String, Option<String>) {
    let header_trimmed = header.trim();
    let parts: Vec<&str> = header_trimmed.split_whitespace().collect();
    let language = parts.first().copied().unwrap_or("text").to_lowercase();

    let mut detected_path = if parts.len() > 1 {
        Some(parts[1].to_string())
    } else if let Some((_, path)) = language.split_once(':') {
        Some(path.to_string())
    } else {
        None
    };

    let clean_language = language.split(':').next().unwrap_or("text").to_string();

    if detected_path.is_none() {
        if let Some(first_line) = code.lines().next() {
            let trimmed = first_line.trim();
            let comment_content = trimmed
                .strip_prefix("//")
                .or_else(|| trimmed.strip_prefix('#'))
                .or_else(|| {
                    trimmed
                        .strip_prefix("/*")
                        .and_then(|s| s.strip_suffix("*/"))
                })
                .or_else(|| {
                    trimmed
                        .strip_prefix("<!--")
                        .and_then(|s| s.strip_suffix("-->"))
                });

            if let Some(candidate) = comment_content {
                let candidate = candidate.trim();
                if candidate.contains('/')
                    && !candidate.contains(' ')
                    && candidate.len() < 120
                    && !candidate.starts_with("http")
                    && !candidate.starts_with('!')
                {
                    detected_path = Some(candidate.to_string());
                }
            }
        }
    }

    (clean_language, detected_path)
}

pub fn extract_markdown_segments(raw: &str) -> Vec<MarkdownSegment> {
    let mut segments = Vec::new();
    let mut current_pos = 0;
    let mut search_from = 0;

    while let Some(start_fence) = raw[search_from..].find("```") {
        let fence_start_idx = search_from + start_fence;

        if !is_fence_line(raw, fence_start_idx) {
            search_from = fence_start_idx + 3;
            continue;
        }

        if fence_start_idx > current_pos {
            let text = &raw[current_pos..fence_start_idx];
            if !text.is_empty() {
                segments.push(MarkdownSegment::Markdown(text.to_string()));
            }
        }

        let header_start = fence_start_idx + 3;
        let Some(header_newline) = raw[header_start..].find('\n') else {
            segments.push(MarkdownSegment::Markdown(
                raw[fence_start_idx..].to_string(),
            ));
            return segments;
        };

        let header_line = raw[header_start..header_start + header_newline].trim();
        let code_start = header_start + header_newline + 1;

        let mut close_fence_idx = None;
        let mut search_pos = code_start;
        while let Some(close_pos) = raw[search_pos..].find("```") {
            let candidate_idx = search_pos + close_pos;
            if is_fence_line(raw, candidate_idx) {
                close_fence_idx = Some(candidate_idx);
                break;
            }
            search_pos = candidate_idx + 3;
        }

        if let Some(close_idx) = close_fence_idx {
            let code = &raw[code_start..close_idx];
            let (language, header_path) = parse_code_block_header(header_line, code);
            segments.push(MarkdownSegment::CodeBlock {
                language,
                header_path,
                code: code.to_string(),
            });
            let after_close = close_idx + 3;
            let next_pos = if after_close < raw.len() && raw.as_bytes()[after_close] == b'\n' {
                after_close + 1
            } else {
                after_close
            };
            current_pos = next_pos;
            search_from = current_pos;
        } else {
            let code = &raw[code_start..];
            let (language, header_path) = parse_code_block_header(header_line, code);
            segments.push(MarkdownSegment::CodeBlock {
                language,
                header_path,
                code: code.to_string(),
            });
            return segments;
        }
    }

    if current_pos < raw.len() {
        let remainder = &raw[current_pos..];
        if !remainder.is_empty() {
            segments.push(MarkdownSegment::Markdown(remainder.to_string()));
        }
    }

    if segments.is_empty() && !raw.is_empty() {
        segments.push(MarkdownSegment::Markdown(raw.to_string()));
    }

    segments
}

struct MarkdownRenderState {
    source: String,
    state: Entity<TextViewState>,
}

const MARKDOWN_CACHE_ENTRY_LIMIT: usize = 512;

fn markdown_cache_exceeded(entry_count: usize) -> bool {
    entry_count > MARKDOWN_CACHE_ENTRY_LIMIT
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TranscriptRow {
    Message(usize),
    Activities(Range<usize>),
    Working,
}

fn is_activity_only(message: &ChatMessageInfo) -> bool {
    message.role == MessageRole::Assistant
        && message.content.is_empty()
        && message.reasoning_content.is_none()
        && message
            .tool_activities
            .iter()
            .any(|activity| activity.title != "update_plan")
}

fn build_transcript_rows(messages: &[ChatMessageInfo], generating: bool) -> Vec<TranscriptRow> {
    let mut rows = Vec::with_capacity(messages.len().saturating_add(1));
    let mut index = 0;
    while index < messages.len() {
        if !is_activity_only(&messages[index]) {
            rows.push(TranscriptRow::Message(index));
            index += 1;
            continue;
        }

        let start = index;
        while index < messages.len() && is_activity_only(&messages[index]) {
            index += 1;
        }
        rows.push(TranscriptRow::Activities(start..index));
    }
    if generating {
        rows.push(TranscriptRow::Working);
    }
    rows
}

fn grouped_tool_activities(
    messages: &[ChatMessageInfo],
) -> impl Iterator<Item = &ToolActivityInfo> + Clone {
    messages
        .iter()
        .flat_map(|message| message.tool_activities.iter())
        .filter(|activity| activity.title != "update_plan")
}

fn subagent_popover_counts(
    statuses: impl IntoIterator<Item = SubagentActivityStatus>,
) -> Option<(usize, usize)> {
    let (count, active_count) = statuses
        .into_iter()
        .fold((0, 0), |(count, active), status| {
            (
                count + 1,
                active
                    + usize::from(matches!(
                        status,
                        SubagentActivityStatus::Queued | SubagentActivityStatus::Running
                    )),
            )
        });
    (count > 0).then_some((count, active_count))
}

fn format_trajectory_raw_json(entry: &TrajectoryEntry) -> String {
    serde_json::to_string_pretty(entry).unwrap_or_else(|_| entry.detail.clone())
}

#[cfg(test)]
fn reconcile_trajectory_entries(
    cached: Vec<TrajectoryEntry>,
    source: &[TrajectoryEntry],
) -> Vec<TrajectoryEntry> {
    reconcile_trajectory_entries_with_append(cached, source).0
}

fn reconcile_trajectory_entries_with_append(
    mut cached: Vec<TrajectoryEntry>,
    source: &[TrajectoryEntry],
) -> (Vec<TrajectoryEntry>, bool) {
    if source.starts_with(&cached) {
        cached.extend_from_slice(&source[cached.len()..]);
        (cached, true)
    } else {
        (source.to_vec(), false)
    }
}

fn reconcile_trajectory_entries_by_epoch(
    mut cached: Vec<TrajectoryEntry>,
    source: &[TrajectoryEntry],
    cached_epoch: u64,
    source_epoch: u64,
) -> (Vec<TrajectoryEntry>, bool) {
    if cached_epoch == source_epoch && source.len() >= cached.len() {
        cached.extend_from_slice(&source[cached.len()..]);
        (cached, true)
    } else {
        reconcile_trajectory_entries_with_append(cached, source)
    }
}

fn contains_case_insensitive(haystack: &str, lowercase_query: &str) -> bool {
    if lowercase_query.is_empty() {
        return true;
    }
    if lowercase_query.is_ascii() && haystack.is_ascii() {
        return haystack
            .as_bytes()
            .windows(lowercase_query.len())
            .any(|window| window.eq_ignore_ascii_case(lowercase_query.as_bytes()));
    }
    haystack.to_lowercase().contains(lowercase_query)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrajectoryCacheKey {
    revision: u64,
    epoch: u64,
    mode: TrajectoryMode,
    query: String,
    category: Option<String>,
    lane: Option<String>,
}

fn extend_trajectory_facets(
    categories: &mut Vec<String>,
    lane_latest: &mut std::collections::BTreeMap<String, String>,
    filtered_indices: &mut Vec<usize>,
    entries: &[TrajectoryEntry],
    start: usize,
    key: &TrajectoryCacheKey,
) {
    for (index, entry) in entries.iter().enumerate().skip(start) {
        if let Err(position) = categories.binary_search(&entry.category) {
            categories.insert(position, entry.category.clone());
        }
        if let Some(lane) = &entry.lane {
            lane_latest.insert(lane.clone(), entry.summary.clone());
        }
        let matches = key
            .category
            .as_ref()
            .is_none_or(|category| &entry.category == category)
            && key
                .lane
                .as_ref()
                .is_none_or(|lane| entry.lane.as_ref() == Some(lane))
            && [
                entry.category.as_str(),
                entry.summary.as_str(),
                entry.detail.as_str(),
                entry.lane.as_deref().unwrap_or(""),
                entry.correlation_id.as_deref().unwrap_or(""),
            ]
            .iter()
            .any(|value| contains_case_insensitive(value, &key.query));
        if matches {
            filtered_indices.push(index);
        }
    }
}

fn extend_trajectory_previews(
    previews: &mut Vec<SharedString>,
    entries: &[TrajectoryEntry],
    start: usize,
) {
    previews.reserve(entries.len().saturating_sub(start));
    previews.extend(entries[start..].iter().map(|entry| {
        if entry.detail.trim().is_empty() {
            entry.summary.clone().into()
        } else {
            format!("{}  {}", entry.summary, entry.detail.replace('\n', " ")).into()
        }
    }));
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TrajectoryRow {
    RequestHeader(u32),
    Setup,
    TurnHeader(u32),
    Entry(usize),
}

fn build_trajectory_rows(
    all_entries: &[TrajectoryEntry],
    filtered_indices: &[usize],
    mode: TrajectoryMode,
) -> Vec<TrajectoryRow> {
    let mut rows = Vec::with_capacity(filtered_indices.len());
    extend_trajectory_rows(&mut rows, all_entries, filtered_indices, 0, mode);
    rows
}

fn extend_trajectory_rows(
    rows: &mut Vec<TrajectoryRow>,
    all_entries: &[TrajectoryEntry],
    filtered_indices: &[usize],
    start: usize,
    mode: TrajectoryMode,
) {
    let previous = start
        .checked_sub(1)
        .and_then(|index| filtered_indices.get(index))
        .map(|&index| &all_entries[index]);
    let mut previous_turn = previous.and_then(|entry| entry.turn);
    let mut previous_request = previous.and_then(|entry| entry.request);
    let mut request_input_seen = previous_request.is_some();
    rows.reserve(filtered_indices.len().saturating_sub(start));
    for &all_index in &filtered_indices[start..] {
        let entry = &all_entries[all_index];
        if mode == TrajectoryMode::Requests && entry.request != previous_request {
            if let Some(request) = entry.request {
                rows.push(TrajectoryRow::RequestHeader(request));
                request_input_seen = false;
            }
            previous_request = entry.request;
        }
        if mode == TrajectoryMode::Requests && entry.request.is_some() && !request_input_seen {
            if entry.category != "Input" {
                rows.push(TrajectoryRow::Setup);
            }
            request_input_seen = true;
        }
        if mode != TrajectoryMode::Requests && entry.turn != previous_turn {
            if let Some(turn) = entry.turn {
                rows.push(TrajectoryRow::TurnHeader(turn));
            }
            previous_turn = entry.turn;
        }
        rows.push(TrajectoryRow::Entry(all_index));
    }
}

#[derive(Default)]
struct TrajectorySummary {
    overview_positions: [HashSet<usize>; 3],
    overview_prefix: [Vec<u32>; 3],
    tool_count: usize,
    total_duration_ms: u64,
    anomaly_count: usize,
    max_turn: u32,
}

fn summarize_trajectory(entries: &[TrajectoryEntry]) -> TrajectorySummary {
    let mut summary = TrajectorySummary::default();
    extend_trajectory_summary(&mut summary, entries);
    summary
}

fn extend_trajectory_summary(summary: &mut TrajectorySummary, entries: &[TrajectoryEntry]) {
    for prefix in &mut summary.overview_prefix {
        if prefix.is_empty() {
            prefix.push(0);
        }
    }
    for entry in entries {
        let groups = [
            matches!(
                entry.category.as_str(),
                "Input" | "Context" | "Context Manifest" | "Queue" | "Request"
            ),
            matches!(
                entry.category.as_str(),
                "Operation" | "Step" | "Retry" | "Turn" | "Error" | "Provider" | "Anomaly"
            ),
            matches!(entry.category.as_str(), "Tool" | "Tool runtime"),
        ];
        for (prefix, present) in summary.overview_prefix.iter_mut().zip(groups) {
            prefix.push(prefix.last().copied().unwrap_or_default() + u32::from(present));
        }
        summary.tool_count += usize::from(groups[2]);
        summary.total_duration_ms = summary
            .total_duration_ms
            .saturating_add(entry.diagnostics.duration_ms.unwrap_or_default());
        summary.anomaly_count +=
            usize::from(entry.diagnostics.is_anomaly || entry.category == "Anomaly");
        summary.max_turn = summary.max_turn.max(entry.turn.unwrap_or_default());
    }
    let entry_count = summary.overview_prefix[0].len().saturating_sub(1);
    for (positions, prefix) in summary
        .overview_positions
        .iter_mut()
        .zip(&summary.overview_prefix)
    {
        positions.clear();
        for position in 0..48 {
            let start = (position * entry_count).div_ceil(48);
            let end = ((position + 1) * entry_count).div_ceil(48);
            if prefix[end] > prefix[start] {
                positions.insert(position);
            }
        }
    }
}

struct TrajectoryRenderCache {
    key: TrajectoryCacheKey,
    all_entries: Vec<TrajectoryEntry>,
    categories: Arc<Vec<String>>,
    lanes: Arc<Vec<String>>,
    lane_latest: Arc<std::collections::BTreeMap<String, String>>,
    filtered_indices: Vec<usize>,
    previews: Vec<SharedString>,
    rows: Vec<TrajectoryRow>,
    summary: TrajectorySummary,
}

pub fn init(cx: &mut App) {
    // gpui-component's Textarea owns the focused `Input` context. Register
    // after gpui-component initialization so this action can inspect image
    // clipboard entries while preserving text paste behavior.
    cx.bind_keys([
        KeyBinding::new("cmd-v", PasteClipboard, Some(INPUT_KEY_CONTEXT)),
        KeyBinding::new("ctrl-v", PasteClipboard, Some(INPUT_KEY_CONTEXT)),
        KeyBinding::new(
            "tab",
            CompleteSlashCommand,
            Some(SLASH_COMMAND_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "up",
            SelectPreviousSlashCommand,
            Some(SLASH_COMMAND_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "down",
            SelectNextSlashCommand,
            Some(SLASH_COMMAND_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "escape",
            DismissSlashCommand,
            Some(SLASH_COMMAND_BINDING_CONTEXT),
        ),
    ]);
}

pub struct ChatListView {
    model: Entity<AppState>,
    pub(crate) input_state: Entity<TextareaState>,
    pub(crate) header_left_padding: Pixels,
    transcript_list_state: ListState,
    transcript_messages: Arc<Vec<ChatMessageInfo>>,
    transcript_rows: Vec<TranscriptRow>,
    transcript_generating: bool,
    trajectory_list_state: ListState,
    expanded_activity_groups: HashSet<String>,
    markdown_states: HashMap<(SharedString, String), MarkdownRenderState>,
    markdown_cache_namespace: SharedString,
    pasted_images: Vec<ImageAttachment>,
    last_session_key: Option<(std::path::PathBuf, String)>,
    initial_scroll_frames: u8,
    current_tab: CentralTab,
    editor: Entity<EditorView>,
    trajectory_mode: TrajectoryMode,
    trajectory_search: String,
    trajectory_search_input: Entity<InputState>,
    trajectory_category: Option<String>,
    trajectory_lane: Option<String>,
    selected_trajectory_index: Option<usize>,
    trajectory_inspector_tab: TrajectoryInspectorTab,
    trajectory_cache: Option<TrajectoryRenderCache>,
    trajectory_raw_json: Option<(u64, usize, String)>,
    slash_command_cache: Option<(
        Option<std::path::PathBuf>,
        std::time::Instant,
        Vec<SlashCommandInfo>,
    )>,
    slash_scroll_handle: ScrollHandle,
    selected_slash_index: usize,
    dismiss_slash_menu: bool,
    permission_details_open: bool,
    context_meter_open: bool,
    subagents_popover_open: bool,
    selected_subagent_run_id: Option<String>,
    _subscriptions: Vec<Subscription>,
}

fn active_slash_command_query(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }
    let rest = &trimmed[1..];
    if rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

async fn next_chat_stream_batch(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<ChatStreamEvent>,
) -> Option<Vec<ChatStreamEvent>> {
    let mut events = vec![receiver.recv().await?];
    while events.len() < 128 {
        let Ok(event) = receiver.try_recv() else {
            break;
        };
        events.push(event);
    }
    Some(events)
}

impl ChatListView {
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let transcript_list_state = ListState::new(0, ListAlignment::Bottom, px(600.0));
        transcript_list_state.set_follow_mode(FollowMode::Tail);
        let trajectory_list_state = ListState::new(0, ListAlignment::Top, px(400.0));
        let input_state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Do anything...")
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .soft_wrap(true)
        });

        let trajectory_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search trajectory…"));
        let mut stream_rx = model
            .update(cx, |state, _cx| state.stream_rx.take())
            .expect("chat stream receiver was already taken");

        let editor = cx.new(|cx| EditorView::new(model.clone(), window, cx));

        let sub1 = cx.observe(&model, |this, model, cx| {
            if let Some(target) =
                model.update(cx, |state, _cx| state.requested_editor_target.take())
            {
                match target {
                    crate::state::RequestedEditorTarget::File { project, path } => {
                        let is_active = {
                            let state = model.read(cx);
                            editor_target_matches_active_work_dir(
                                &project,
                                state.active_git_work_dir().as_deref(),
                            )
                        };
                        if is_active {
                            this.current_tab = CentralTab::Editor;
                            this.editor.update(cx, |editor, cx| {
                                editor.open_file(project, &path, cx);
                            });
                        }
                    }
                    crate::state::RequestedEditorTarget::Diff {
                        project,
                        path,
                        content,
                    } => {
                        let is_active = {
                            let state = model.read(cx);
                            editor_target_matches_active_work_dir(
                                &project,
                                state.active_git_work_dir().as_deref(),
                            )
                        };
                        if is_active {
                            this.current_tab = CentralTab::Editor;
                            this.editor.update(cx, |editor, cx| {
                                editor.open_diff(&path, &content, cx);
                            });
                        }
                    }
                }
            }
            cx.notify();
        });

        let sub_editor = cx.observe(&editor, |_this, _editor, cx| {
            cx.notify();
        });

        let model_clone = model.clone();
        let submit_list_state = transcript_list_state.clone();
        let sub2 = cx.subscribe_in(
            &input_state,
            window,
            move |this, input_state, event: &InputEvent, window, cx| {
                cx.notify();
                match event {
                    InputEvent::Change => {
                        this.dismiss_slash_menu = false;
                        this.selected_slash_index = 0;
                        this.slash_scroll_handle.scroll_to_item(0);
                    }
                    InputEvent::PressEnter {
                        secondary,
                        shift: false,
                    } => {
                        let text = input_state.read(cx).value().to_string();
                        let is_generating = model_clone.read(cx).is_generating;
                        let project_root = model_clone.read(cx).active_work_dir.clone();

                        if let Some(query) = active_slash_command_query(&text) {
                            if !this.dismiss_slash_menu {
                                let matching = this
                                    .cached_slash_commands(project_root.as_deref())
                                    .into_iter()
                                    .filter(|cmd| query.is_empty() || cmd.name.starts_with(query))
                                    .collect::<Vec<_>>();
                                if !matching.is_empty() {
                                    let selected = this
                                        .selected_slash_index
                                        .min(matching.len().saturating_sub(1));
                                    let command_name = matching[selected].name.clone();
                                    this.complete_slash_command(&command_name, window, cx);
                                    return;
                                }
                            }
                        }

                        if !text.trim().is_empty()
                            || (!is_generating && !this.pasted_images.is_empty())
                        {
                            let images = std::mem::take(&mut this.pasted_images);
                            let is_steer = *secondary;
                            model_clone.update(cx, |state, cx| {
                                if is_generating {
                                    controller::dispatch(
                                        state,
                                        AppAction::StageBusyMessage { text, images },
                                    );
                                    if is_steer {
                                        controller::dispatch(state, AppAction::SteerPendingMessage);
                                    } else {
                                        controller::dispatch(state, AppAction::QueuePendingMessage);
                                    }
                                } else {
                                    controller::dispatch(
                                        state,
                                        AppAction::SendPromptWithImages { text, images },
                                    );
                                }
                                cx.notify();
                            });
                            input_state.update(cx, |state, cx| {
                                state.set_value("", window, cx);
                            });
                            submit_list_state.scroll_to_end();
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            },
        );

        let stream_model = model.clone();
        cx.spawn(async move |this, cx| {
            while let Some(events) = next_chat_stream_batch(&mut stream_rx).await {
                let changed = stream_model.update(cx, |state, cx| {
                    let changed = state.drain_chat_stream(events);
                    if changed {
                        cx.notify();
                    }
                    changed
                });
                cx.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
                if changed && stream_rx.is_empty() {
                    let _ = this.update(cx, |_this, cx| cx.notify());
                }
            }
        })
        .detach();

        let sub3 = cx.observe(&trajectory_search_input, |this, input, cx| {
            this.trajectory_search = input.read(cx).value().to_string();
            cx.notify();
        });

        Self {
            model,
            input_state,
            header_left_padding: px(14.0),
            transcript_list_state,
            transcript_messages: Arc::new(Vec::new()),
            transcript_rows: Vec::new(),
            transcript_generating: false,
            trajectory_list_state,
            expanded_activity_groups: HashSet::new(),
            markdown_states: HashMap::new(),
            markdown_cache_namespace: SharedString::from(""),
            pasted_images: Vec::new(),
            last_session_key: None,
            initial_scroll_frames: 0,
            current_tab: CentralTab::Chat,
            editor,
            trajectory_mode: TrajectoryMode::Execution,
            trajectory_search: String::new(),
            trajectory_search_input,
            trajectory_category: None,
            trajectory_lane: None,
            selected_trajectory_index: None,
            trajectory_inspector_tab: TrajectoryInspectorTab::Overview,
            trajectory_cache: None,
            trajectory_raw_json: None,
            slash_command_cache: None,
            slash_scroll_handle: ScrollHandle::new(),
            selected_slash_index: 0,
            dismiss_slash_menu: false,
            permission_details_open: false,
            context_meter_open: false,
            subagents_popover_open: false,
            selected_subagent_run_id: None,
            _subscriptions: vec![sub1, sub2, sub3, sub_editor],
        }
    }

    fn paste_composer_clipboard(
        &mut self,
        _action: &PasteClipboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(clipboard) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = clipboard.text().filter(|text| !text.is_empty()) {
            self.input_state.update(cx, |input, cx| {
                input.insert(text, window, cx);
            });
        }

        let mut pasted = 0;
        for entry in clipboard.entries {
            let ClipboardEntry::Image(image) = entry else {
                continue;
            };
            if image.bytes.is_empty() {
                continue;
            }

            let mime_type = image.format.mime_type();
            let extension = mime_type.strip_prefix("image/").unwrap_or("png");
            self.pasted_images.push(ImageAttachment {
                display_name: format!("Pasted image {}.{extension}", self.pasted_images.len() + 1),
                data_url: format!(
                    "data:{mime_type};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(image.bytes)
                ),
            });
            pasted += 1;
        }

        cx.stop_propagation();
        if pasted > 0 {
            cx.notify();
        }
    }

    pub(crate) fn set_tab(&mut self, tab: CentralTab, cx: &mut Context<Self>) {
        self.current_tab = tab;
        cx.notify();
    }

    pub(crate) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.current_tab = CentralTab::Chat;
        self.input_state.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn set_composer_text(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.current_tab = CentralTab::Chat;
        self.input_state.update(cx, |input, cx| {
            input.set_value(text, window, cx);
        });
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_title = {
            let state = self.model.read(cx);
            state
                .projects
                .iter()
                .flat_map(|project| project.sessions.iter())
                .find(|session| state.active_session_id.as_deref() == Some(&session.id))
                .map(|session| session.title.clone())
                .unwrap_or_else(|| "New task".to_string())
        };
        let theme = cx.theme().colors;
        let editor_tab_count = self.editor.read(cx).tab_count();
        let editor_label = if editor_tab_count > 0 {
            format!("Editor ({editor_tab_count})")
        } else {
            "Editor".to_string()
        };

        div()
            .h(px(52.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .pl(self.header_left_padding)
            .pr(px(128.0))
            .border_b_1()
            .border_color(theme.title_bar_border)
            .bg(theme.title_bar)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_start()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.0))
                            .line_height(px(18.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(active_title),
                    ),
            )
            .child(
                div()
                    .h(px(30.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p(px(2.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted.opacity(0.4))
                    .child(
                        Button::new("trajectory-tab-events")
                            .icon(Icon::default().path("icons/tabs/trajectory.svg"))
                            .label("Trajectory")
                            .tooltip("Trajectory (Execution & Diagnostics)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Trajectory)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Trajectory;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("trajectory-tab-chat")
                            .icon(Icon::default().path("icons/tabs/chat.svg"))
                            .label("Chat")
                            .tooltip("Chat (Conversation & Turn History)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Chat)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Chat;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("trajectory-tab-editor")
                            .icon(Icon::default().path("icons/tabs/editor.svg"))
                            .label(editor_label)
                            .tooltip("Editor (Code & Diff Review)")
                            .ghost()
                            .xsmall()
                            .selected(self.current_tab == CentralTab::Editor)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.current_tab = CentralTab::Editor;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1())
    }

    /// Renders the 16px status circle used for a plan step: a bordered ✓ for
    /// completed, a spinner for in-progress (active generation), a static dot for in-progress (idle), and an empty ring for pending.
    fn plan_step_marker(
        status: PlanItemStatus,
        is_generating: bool,
        colors: gpui_component::ThemeColor,
    ) -> AnyElement {
        match status {
            PlanItemStatus::Completed => div()
                .size(px(16.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(colors.success)
                .text_size(px(10.0))
                .font_weight(FontWeight::BOLD)
                .text_color(colors.success)
                .child("✓")
                .into_any_element(),
            PlanItemStatus::InProgress => {
                if is_generating {
                    div()
                        .size(px(16.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(colors.primary)
                        .child(gpui_component::spinner::Spinner::new().xsmall())
                        .into_any_element()
                } else {
                    div()
                        .size(px(16.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(colors.primary)
                        .child(div().size(px(6.0)).rounded_full().bg(colors.primary))
                        .into_any_element()
                }
            }
            PlanItemStatus::Pending => div()
                .size(px(16.0))
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(colors.muted_foreground)
                .into_any_element(),
        }
    }

    fn render_plan_tracker(
        &self,
        plan: &SessionPlan,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if plan.items.is_empty() {
            return None;
        }

        let is_generating = self.model.read(cx).is_generating;
        let theme = cx.theme().colors;
        let completed = plan
            .items
            .iter()
            .filter(|item| item.status == PlanItemStatus::Completed)
            .count();
        let total = plan.items.len();
        let current_step = plan
            .items
            .iter()
            .position(|item| item.status == PlanItemStatus::InProgress)
            .or_else(|| {
                plan.items
                    .iter()
                    .position(|item| item.status == PlanItemStatus::Pending)
            })
            .map(|index| index + 1)
            .unwrap_or(total);
        let is_complete = completed == total;
        let content_plan = plan.clone();

        Some(
            HoverCard::new("session-plan-hover-card")
                .w_full()
                .flex_none()
                .anchor(Anchor::BottomCenter)
                .close_delay(Duration::from_millis(700))
                .trigger(
                    div().w_full().flex().justify_center().py_1().child(
                        Button::new("session-plan-tracker").ghost().child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Self::plan_step_marker(
                                    if is_complete {
                                        PlanItemStatus::Completed
                                    } else {
                                        PlanItemStatus::InProgress
                                    },
                                    is_generating,
                                    theme,
                                ))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.muted_foreground)
                                        .child(format!("Step {current_step} / {total}")),
                                ),
                        ),
                    ),
                )
                .content(move |_state, _window, _cx| {
                    let colors = theme;
                    let rows = content_plan.items.iter().enumerate().map(|(index, item)| {
                        let marker = Self::plan_step_marker(item.status, is_generating, colors);
                        div().flex().items_start().gap_2().child(marker).child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .text_sm()
                                .text_color(colors.foreground)
                                .child(format!("{}. {}", index + 1, item.step)),
                        )
                    });
                    div()
                        .w(px(520.0))
                        .max_w(px(CHAT_CONTENT_MAX_WIDTH - 32.0))
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(content_plan.explanation.clone().map(|explanation| {
                            div()
                                .flex_none()
                                .pb_2()
                                .border_b_1()
                                .border_color(colors.border)
                                .text_sm()
                                .text_color(colors.muted_foreground)
                                .child(explanation)
                        }))
                        .child(
                            div()
                                .w_full()
                                .max_h(px(280.0))
                                .flex()
                                .flex_col()
                                .gap_2()
                                .overflow_y_scrollbar()
                                .children(rows),
                        )
                })
                .into_any_element(),
        )
    }

    fn render_tool_activity(
        &self,
        activity: &ToolActivityInfo,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (marker, marker_color) = match activity.category.as_str() {
            "Error" => ("!", theme.danger),
            "Working" | "Thinking" => ("◌", theme.primary),
            "Completed" | "Edited" | "Created" | "Ran" | "Loaded" => ("✓", theme.success),
            _ => ("✓", theme.muted_foreground),
        };
        let model = self.model.clone();
        let tool_call_id = activity.id.clone();
        let has_detail = !activity.detail.trim().is_empty();
        let row_id = SharedString::from(activity.id.clone());
        let display_summary = activity.display_summary.clone();

        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .py_1()
            .child(
                div()
                    .id(row_id)
                    .h(px(28.0))
                    .px_1()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(has_detail, |row| {
                        row.cursor_pointer()
                            .hover(|row| row.bg(theme.muted))
                            .on_click(move |_event, _window, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::ToggleToolActivity(tool_call_id.clone()),
                                    );
                                    cx.notify();
                                });
                            })
                    })
                    .child({
                        let marker_el = div()
                            .w(px(18.0))
                            .flex_none()
                            .text_center()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(marker_color)
                            .child(marker);
                        marker_el.into_any_element()
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(display_summary),
                    )
                    .children(has_detail.then(|| {
                        Icon::new(if activity.is_expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground)
                    })),
            )
            .children(activity.is_expanded.then(|| {
                div()
                    .ml(px(26.0))
                    .mt_1()
                    .p_2()
                    .max_h(px(240.0))
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .overflow_y_scrollbar()
                    .child(activity.detail.clone())
            }))
    }

    fn render_activity_group(
        &mut self,
        messages: &[ChatMessageInfo],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const RECENT_ACTIVITY_LIMIT: usize = 4;

        let theme = cx.theme().colors;
        let activities = grouped_tool_activities(messages);
        let group_id = activities
            .clone()
            .next()
            .map(|activity| activity.id.clone())
            .unwrap_or_else(|| "empty".into());
        let is_expanded = self.expanded_activity_groups.contains(&group_id);
        let hidden_count = activities
            .clone()
            .count()
            .saturating_sub(RECENT_ACTIVITY_LIMIT);
        let visible_start = if is_expanded { 0 } else { hidden_count };
        let activity_rows = activities
            .skip(visible_start)
            .map(|activity| self.render_tool_activity(activity, cx))
            .collect::<Vec<_>>();
        let button_group_id = group_id.clone();

        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .my_1()
            .px_4()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .children((hidden_count > 0).then(|| {
                        Button::new(SharedString::from(format!("activity-group-{group_id}")))
                            .xsmall()
                            .ghost()
                            .justify_start()
                            .text_color(theme.muted_foreground)
                            .label(if is_expanded {
                                "Collapse earlier activities".to_string()
                            } else {
                                format!("{hidden_count} earlier activities")
                            })
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                if !this.expanded_activity_groups.remove(&button_group_id) {
                                    this.expanded_activity_groups
                                        .insert(button_group_id.clone());
                                }
                                cx.notify();
                            }))
                    }))
                    .children(activity_rows),
            )
            .into_any_element()
    }

    fn render_working_indicator(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .my_1()
            .px_4()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary))
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary))
                            .child(div().size(px(4.5)).rounded_full().bg(theme.primary)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Working…"),
                    ),
            )
            .into_any_element()
    }

    fn render_trajectory_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self
            .trajectory_cache
            .as_ref()
            .and_then(|cache| cache.rows.get(index))
            .cloned()
        else {
            return Empty.into_any_element();
        };
        let theme = cx.theme().colors;
        match row {
            TrajectoryRow::RequestHeader(request) => div()
                .h(px(28.0))
                .px_3()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.border.opacity(0.65))
                .bg(theme.muted.opacity(0.35))
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.accent)
                .child(format!("Request #{request}"))
                .into_any_element(),
            TrajectoryRow::Setup => div()
                .h(px(20.0))
                .px_3()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.border.opacity(0.35))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child("Setup")
                .into_any_element(),
            TrajectoryRow::TurnHeader(turn) => div()
                .h(px(22.0))
                .px_3()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(theme.border.opacity(0.5))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("Turn {turn}"))
                .into_any_element(),
            TrajectoryRow::Entry(all_index) => {
                let entry = &self
                    .trajectory_cache
                    .as_ref()
                    .expect("trajectory cache")
                    .all_entries[all_index];
                let selected = Some(all_index) == self.selected_trajectory_index;
                let preview = self
                    .trajectory_cache
                    .as_ref()
                    .expect("trajectory cache")
                    .previews[all_index]
                    .clone();
                let (badge_bg, badge_fg, badge_label): (Hsla, Hsla, SharedString) =
                    match entry.category.as_str() {
                        "Tool" | "Tool runtime" => {
                            (theme.warning.opacity(0.18), theme.warning, "TOOL".into())
                        }
                        "Provider" => (
                            theme.primary.opacity(0.18),
                            theme.primary,
                            "PROVIDER".into(),
                        ),
                        "Context Manifest" | "Manifest" => (
                            theme.accent.opacity(0.14),
                            theme.muted_foreground,
                            "MANIFEST".into(),
                        ),
                        "Request" => (theme.primary.opacity(0.16), theme.accent, "REQUEST".into()),
                        "Anomaly" => (theme.warning.opacity(0.20), theme.warning, "ANOMALY".into()),
                        "Error" => (theme.danger.opacity(0.20), theme.danger, "ERROR".into()),
                        "Input" => (theme.muted.opacity(0.8), theme.foreground, "INPUT".into()),
                        "Assistant" => (
                            theme.muted.opacity(0.8),
                            theme.foreground,
                            "ASSISTANT".into(),
                        ),
                        "Permission" => (
                            theme.warning.opacity(0.18),
                            theme.warning,
                            "PERMISSION".into(),
                        ),
                        "Subagent" => (
                            theme.primary.opacity(0.16),
                            theme.primary,
                            "SUBAGENT".into(),
                        ),
                        _ => (
                            theme.muted.opacity(0.5),
                            theme.muted_foreground,
                            entry.category.clone().into(),
                        ),
                    };
                let dot_color = if entry.diagnostics.is_anomaly || entry.category == "Anomaly" {
                    theme.warning
                } else if entry.category == "Error"
                    || entry.detail.contains("Failed")
                    || entry.detail.contains("Error")
                    || matches!(
                        entry.diagnostics.status.as_deref(),
                        Some("Failed" | "failed")
                    )
                {
                    theme.danger
                } else if entry.category == "Tool" || entry.category == "Tool runtime" {
                    theme.warning
                } else if entry.category == "Request" {
                    theme.primary
                } else {
                    theme.muted_foreground
                };
                let seq = entry.seq;
                let exit_code = entry.diagnostics.exit_code;
                let duration_ms = entry.diagnostics.duration_ms;
                let lane = entry.lane.clone();
                let view = cx.entity().clone();
                div()
                    .id(SharedString::from(format!("trajectory-{all_index}")))
                    .h(px(34.0))
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border.opacity(0.45))
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(theme.accent.opacity(0.16))
                            .border_l_2()
                            .border_color(theme.accent)
                    })
                    .hover(|style| style.bg(theme.muted.opacity(0.65)))
                    .child(div().size(px(6.0)).flex_none().rounded_full().bg(dot_color))
                    .child(
                        div()
                            .w(px(84.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .bg(badge_bg)
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(badge_fg)
                            .child(badge_label),
                    )
                    .child(div().min_w_0().flex_1().text_sm().truncate().child(preview))
                    .children(exit_code.map(|code| {
                        let is_ok = code == 0;
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(if is_ok {
                                theme.success.opacity(0.15)
                            } else {
                                theme.danger.opacity(0.15)
                            })
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_ok { theme.success } else { theme.danger })
                            .child(format!("exit {code}"))
                    }))
                    .children(duration_ms.map(|duration| {
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(theme.muted.opacity(0.8))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if duration < 1000 {
                                format!("{duration}ms")
                            } else {
                                format!("{:.1}s", duration as f64 / 1000.0)
                            })
                    }))
                    .children(lane.map(|lane| {
                        div()
                            .max_w(px(110.0))
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(lane)
                    }))
                    .children(seq.map(|seq| {
                        div()
                            .w(px(52.0))
                            .text_right()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("#{seq}"))
                    }))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.selected_trajectory_index = Some(all_index);
                            this.trajectory_inspector_tab = TrajectoryInspectorTab::Overview;
                            cx.notify();
                        })
                    })
                    .into_any_element()
            }
        }
    }

    fn render_trajectory(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (revision, epoch) = match self.trajectory_mode {
            TrajectoryMode::Execution | TrajectoryMode::Requests => {
                let state = self.model.read(cx);
                (state.trajectory_revision(), state.trajectory_epoch())
            }
            TrajectoryMode::ModelContext
            | TrajectoryMode::DurableEvents
            | TrajectoryMode::Recovery => {
                let revision = self.model.read(cx).diagnostics_revision();
                (revision, revision)
            }
        };
        let key = TrajectoryCacheKey {
            revision,
            epoch,
            mode: self.trajectory_mode,
            query: self.trajectory_search.to_lowercase(),
            category: self.trajectory_category.clone(),
            lane: self.trajectory_lane.clone(),
        };
        if self
            .trajectory_cache
            .as_ref()
            .is_none_or(|cache| cache.key != key)
        {
            let cached_len = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.all_entries.len());
            let cached_filtered_len = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.filtered_indices.len());
            let projection_matches = self.trajectory_cache.as_ref().is_some_and(|cache| {
                cache.key.epoch == key.epoch
                    && cache.key.mode == key.mode
                    && cache.key.query == key.query
                    && cache.key.category == key.category
                    && cache.key.lane == key.lane
            });
            let mut cached_summary = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.summary))
                .unwrap_or_default();
            let mut cached_categories = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.categories))
                .unwrap_or_default();
            let mut cached_lane_latest = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.lane_latest))
                .unwrap_or_default();
            let mut cached_filtered_indices = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.filtered_indices))
                .unwrap_or_default();
            let mut cached_previews = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.previews))
                .unwrap_or_default();
            let cached_entries = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.all_entries))
                .unwrap_or_default();
            let cached_epoch = self
                .trajectory_cache
                .as_ref()
                .map_or(epoch, |cache| cache.key.epoch);
            let (all_entries, appended) = match self.trajectory_mode {
                TrajectoryMode::Execution | TrajectoryMode::Requests => {
                    let state = self.model.read(cx);
                    reconcile_trajectory_entries_by_epoch(
                        cached_entries,
                        state.active_trajectory(),
                        cached_epoch,
                        epoch,
                    )
                }
                TrajectoryMode::ModelContext => {
                    let source = self.model.read(cx).active_model_context_diagnostics();
                    reconcile_trajectory_entries_with_append(cached_entries, &source)
                }
                TrajectoryMode::DurableEvents => {
                    let source = self.model.read(cx).active_durable_event_diagnostics();
                    reconcile_trajectory_entries_with_append(cached_entries, &source)
                }
                TrajectoryMode::Recovery => {
                    let source = self.model.read(cx).active_recovery_diagnostics();
                    reconcile_trajectory_entries_with_append(cached_entries, &source)
                }
            };
            let (categories, lane_latest, filtered_indices) = if projection_matches && appended {
                extend_trajectory_facets(
                    Arc::make_mut(&mut cached_categories),
                    Arc::make_mut(&mut cached_lane_latest),
                    &mut cached_filtered_indices,
                    &all_entries,
                    cached_len,
                    &key,
                );
                (
                    cached_categories,
                    cached_lane_latest,
                    cached_filtered_indices,
                )
            } else {
                let mut categories = Vec::new();
                let mut lane_latest = std::collections::BTreeMap::new();
                let mut filtered_indices = Vec::new();
                extend_trajectory_facets(
                    &mut categories,
                    &mut lane_latest,
                    &mut filtered_indices,
                    &all_entries,
                    0,
                    &key,
                );
                (
                    Arc::new(categories),
                    Arc::new(lane_latest),
                    filtered_indices,
                )
            };
            let lanes = Arc::new(lane_latest.keys().cloned().collect());
            let previews = if projection_matches && appended {
                extend_trajectory_previews(&mut cached_previews, &all_entries, cached_len);
                cached_previews
            } else {
                let mut previews = Vec::with_capacity(all_entries.len());
                extend_trajectory_previews(&mut previews, &all_entries, 0);
                previews
            };
            let (rows, extends_previous) = if projection_matches && appended {
                let mut rows = self
                    .trajectory_cache
                    .as_mut()
                    .map(|cache| std::mem::take(&mut cache.rows))
                    .unwrap_or_default();
                extend_trajectory_rows(
                    &mut rows,
                    &all_entries,
                    &filtered_indices,
                    cached_filtered_len,
                    self.trajectory_mode,
                );
                (rows, true)
            } else {
                let rows =
                    build_trajectory_rows(&all_entries, &filtered_indices, self.trajectory_mode);
                let extends_previous = self
                    .trajectory_cache
                    .as_ref()
                    .is_some_and(|cache| rows.starts_with(&cache.rows));
                (rows, extends_previous)
            };
            let summary = if projection_matches && appended {
                extend_trajectory_summary(&mut cached_summary, &all_entries[cached_len..]);
                cached_summary
            } else {
                summarize_trajectory(&all_entries)
            };
            let previous_row_count = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.rows.len());
            if extends_previous {
                self.trajectory_list_state.splice(
                    previous_row_count..previous_row_count,
                    rows.len() - previous_row_count,
                );
            } else {
                self.trajectory_list_state.reset(rows.len());
            }
            self.trajectory_raw_json = None;
            self.trajectory_cache = Some(TrajectoryRenderCache {
                key,
                all_entries,
                categories,
                lanes,
                lane_latest,
                filtered_indices,
                previews,
                rows,
                summary,
            });
        }
        let inspector_tab = self.trajectory_inspector_tab;
        let selected_index = self.selected_trajectory_index;
        if let Some(index) = (inspector_tab == TrajectoryInspectorTab::Raw)
            .then_some(selected_index)
            .flatten()
        {
            let needs_raw = self.trajectory_raw_json.as_ref().is_none_or(
                |(cached_revision, cached_index, _)| {
                    *cached_revision != revision || *cached_index != index
                },
            );
            if needs_raw {
                self.trajectory_raw_json = self
                    .trajectory_cache
                    .as_ref()
                    .and_then(|cache| cache.all_entries.get(index))
                    .map(|entry| (revision, index, format_trajectory_raw_json(entry)));
            }
        }
        let cache = self.trajectory_cache.as_ref().expect("trajectory cache");
        let all_entries = &cache.all_entries;
        let categories = Arc::clone(&cache.categories);
        let lanes = Arc::clone(&cache.lanes);
        let lane_latest = Arc::clone(&cache.lane_latest);
        let entries = &cache.filtered_indices;
        let theme = cx.theme().colors;
        if entries.is_empty() {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No canonical trajectory events have been observed in this session yet.")
                .into_any_element();
        }
        let selected_entry = selected_index
            .and_then(|index| all_entries.get(index))
            .cloned();
        let selected_raw_json = (inspector_tab == TrajectoryInspectorTab::Raw)
            .then(|| {
                self.trajectory_raw_json
                    .as_ref()
                    .map(|(_, _, raw)| raw.clone())
            })
            .flatten();
        let inspector = selected_entry.map(|entry| {
            let close_view = cx.entity().clone();
            let inspector_view = cx.entity().clone();
            let model_visible = entry.diagnostics.model_visible || matches!(
                entry.category.as_str(),
                "Input" | "Assistant" | "Context" | "Context Manifest" | "Tool"
            );
            let provenance = match entry.category.as_str() {
                "Input" => "User transcript · model-visible",
                "Assistant" => "Assistant transcript · model-visible",
                "Context" | "Context Manifest" => "Runtime context package · model-visible",
                "Tool" | "Tool runtime" => "Tool transcript · model-visible",
                "Anomaly" => "Automated diagnostic anomaly · durable",
                "Error" => "Runtime diagnostic · durable",
                _ => "Runtime lifecycle record · durable",
            };
            let mut metadata_items = vec![
                entry.seq.map(|value| ("Sequence", format!("#{value}"))),
                entry.request.map(|value| ("Request", format!("#{value}"))),
                entry.turn.map(|value| ("Turn", value.to_string())),
                entry.run_id.clone().map(|value| ("Run", value)),
                entry.lane.clone().map(|value| ("Lane", value)),
                entry.correlation_id.clone().map(|value| ("Call / Correlation", value)),
                entry.diagnostics.status.clone().map(|value| ("Status", value)),
                entry.diagnostics.duration_ms.map(|value| {
                    (
                        "Duration",
                        if value < 1000 {
                            format!("{value} ms")
                        } else {
                            format!("{:.2} s", value as f64 / 1000.0)
                        },
                    )
                }),
                entry.diagnostics.exit_code.map(|value| ("Exit Code", value.to_string())),
                entry.diagnostics.output_bytes.map(|value| ("Output Size", format!("{value} bytes"))),
                entry.diagnostics.token_estimate.map(|value| ("Est. Tokens", format!("~{value}"))),
                entry.diagnostics.items_count.map(|value| ("Item Count", value.to_string())),
            ];
            if !entry.diagnostics.files_mutated.is_empty() {
                metadata_items.push(Some(("Files Mutated", entry.diagnostics.files_mutated.join(", "))));
            }
            if !entry.diagnostics.commands_executed.is_empty() {
                metadata_items.push(Some(("Commands Executed", entry.diagnostics.commands_executed.join(", "))));
            }
            let metadata = metadata_items.into_iter().flatten();
            let (header_bg, header_fg, header_tag): (Hsla, Hsla, SharedString) = match entry.category.as_str() {
                "Tool" | "Tool runtime" => (theme.warning.opacity(0.18), theme.warning, "TOOL".into()),
                "Provider" => (theme.primary.opacity(0.18), theme.primary, "PROVIDER".into()),
                "Context Manifest" | "Manifest" => (theme.accent.opacity(0.14), theme.muted_foreground, "MANIFEST".into()),
                "Request" => (theme.primary.opacity(0.16), theme.accent, "REQUEST".into()),
                "Anomaly" => (theme.warning.opacity(0.20), theme.warning, "ANOMALY".into()),
                "Error" => (theme.danger.opacity(0.20), theme.danger, "ERROR".into()),
                _ => (theme.muted.opacity(0.5), theme.muted_foreground, entry.category.clone().into()),
            };
            div()
                .w(px(410.0))
                .min_w(px(320.0))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(
                    div()
                        .h(px(48.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(header_bg)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(header_fg)
                                .child(header_tag),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(entry.summary.clone()),
                        )
                        .children(entry.diagnostics.duration_ms.map(|dur| {
                            let dur_str = if dur < 1000 { format!("{dur}ms") } else { format!("{:.1}s", dur as f64 / 1000.0) };
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme.muted.opacity(0.7))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(dur_str)
                        }))
                        .child(
                            Button::new("copy-trajectory-row")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Copy)
                                .tooltip("Copy trajectory entry")
                                .on_click({
                                    let text = format!(
                                        "seq:{:?} turn:{:?} category:{} summary:{} detail:{} lane:{:?} run:{:?} call:{:?}",
                                        entry.seq, entry.turn, entry.category, entry.summary,
                                        entry.detail, entry.lane, entry.run_id, entry.correlation_id,
                                    );
                                    move |_, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                        window.push_notification(
                                            Notification::info("Copied trajectory entry"),
                                            cx,
                                        );
                                    }
                                }),
                        )
                        .child(
                            Button::new("close-trajectory-inspector")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip("Close inspector")
                                .on_click(move |_, _, cx| {
                                    close_view.update(cx, |this, cx| {
                                        this.selected_trajectory_index = None;
                                        cx.notify();
                                    })
                                }),
                        ),
                )
                .child(
                    div()
                        .h(px(38.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .children([
                            ("Overview", TrajectoryInspectorTab::Overview),
                            ("Preview", TrajectoryInspectorTab::Preview),
                            ("Raw", TrajectoryInspectorTab::Raw),
                            ("Source", TrajectoryInspectorTab::Source),
                        ]
                        .into_iter()
                        .map(|(label, tab)| {
                            let view = inspector_view.clone();
                            Button::new(SharedString::from(format!("trajectory-inspector-{label}")))
                                .ghost()
                                .small()
                                .selected(inspector_tab == tab)
                                .label(label)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_inspector_tab = tab;
                                        cx.notify();
                                    })
                                })
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(match inspector_tab {
                            TrajectoryInspectorTab::Overview => div()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(
                                    div()
                                        .p_3()
                                        .rounded_lg()
                                        .bg(theme.muted.opacity(0.3))
                                        .border_1()
                                        .border_color(theme.border.opacity(0.5))
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .children(metadata.map(|(label, value)| {
                                            div()
                                                .flex()
                                                .gap_2()
                                                .text_sm()
                                                .child(
                                                    div()
                                                        .w(px(110.0))
                                                        .flex_none()
                                                        .text_xs()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(theme.muted_foreground)
                                                        .child(label),
                                                )
                                                .child(div().min_w_0().flex_1().text_xs().child(value.clone()))
                                        })),
                                )
                                .children(entry.diagnostics.raw.as_ref().map(|raw_args| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("INPUT ARGUMENTS"))
                                        .child(TextView::markdown(
                                            format!("trajectory-args-{}", entry.seq.unwrap_or(0)),
                                            format!("```json\n{}\n```", raw_args),
                                        ).selectable(true))
                                }))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("VISIBILITY"))
                                .child(div().text_sm().child(if model_visible { "Model-visible transcript/context" } else { "Runtime-only durable diagnostic" }))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("SUMMARY"))
                                .child(div().text_sm().child(entry.summary.clone()))
                                .into_any_element(),
                            TrajectoryInspectorTab::Preview => div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("OUTPUT PREVIEW"))
                                .child(
                                    if entry.detail.is_empty() {
                                        div().text_sm().child("No preview content is available for this event.").into_any_element()
                                    } else {
                                        TextView::markdown(
                                            format!("trajectory-preview-{}", entry.seq.unwrap_or(0)),
                                            entry.detail.clone(),
                                        )
                                        .selectable(true)
                                        .into_any_element()
                                    },
                                )
                                .into_any_element(),
                            TrajectoryInspectorTab::Raw => {
                                let raw_json = selected_raw_json.clone().unwrap_or_default();
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("CANONICAL PROJECTION (JSON)"))
                                    .child(TextView::markdown(
                                        format!("trajectory-raw-{}", entry.seq.unwrap_or(0)),
                                        format!("```json\n{raw_json}\n```"),
                                    ).selectable(true))
                                    .into_any_element()
                            }
                            TrajectoryInspectorTab::Source => div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("PROVENANCE"))
                                .child(div().text_sm().child(entry.diagnostics.source.clone().unwrap_or_else(|| provenance.to_string())))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("LINEAGE"))
                                .child(div().text_sm().child(format!(
                                    "Request {} · Turn {} · Lane {}",
                                    entry.request.map_or("—".to_string(), |request| format!("#{request}")),
                                    entry.turn.map_or("—".to_string(), |turn| turn.to_string()),
                                    entry.lane.as_deref().unwrap_or("—"),
                                )))
                                .children(entry.diagnostics.parent_id.as_ref().map(|p| {
                                    div().flex().flex_col().gap_1()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("PARENT ENTRY"))
                                        .child(div().text_sm().font_family("monospace").child(p.clone()))
                                }))
                                .children(entry.diagnostics.result_id.as_ref().map(|r| {
                                    div().flex().flex_col().gap_1()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("RESULT ENTRY"))
                                        .child(div().text_sm().font_family("monospace").child(r.clone()))
                                }))
                                .children(entry.correlation_id.clone().map(|id| div().text_sm().child(format!("Correlation: {id}"))))
                                .into_any_element(),
                        }),
                )
        });
        let overview_lane = |label: &'static str, markers: &HashSet<usize>, color: Hsla| {
            div()
                .h(px(18.0))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(48.0))
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(12.0))
                        .flex()
                        .items_end()
                        .gap(px(2.0))
                        .children((0..48).map(|index| {
                            div()
                                .flex_1()
                                .h(if markers.contains(&index) {
                                    px(10.0)
                                } else {
                                    px(2.0)
                                })
                                .rounded_sm()
                                .bg(if markers.contains(&index) {
                                    color
                                } else {
                                    theme.border.opacity(0.35)
                                })
                        })),
                )
        };
        let overview = div()
            .h(px(58.0))
            .flex_none()
            .flex()
            .flex_col()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(overview_lane(
                "Input",
                &cache.summary.overview_positions[0],
                theme.success,
            ))
            .child(overview_lane(
                "Model",
                &cache.summary.overview_positions[1],
                theme.primary,
            ))
            .child(overview_lane(
                "Tools",
                &cache.summary.overview_positions[2],
                theme.warning,
            ));
        let category_label = self
            .trajectory_category
            .clone()
            .unwrap_or_else(|| "All events".into());
        let lane_label = self
            .trajectory_lane
            .clone()
            .unwrap_or_else(|| format!("{} lanes", lanes.len()));
        let category_view = cx.entity().clone();
        let lane_view = cx.entity().clone();
        let mode_view = cx.entity().clone();
        let mode_label = match self.trajectory_mode {
            TrajectoryMode::Execution => "Execution",
            TrajectoryMode::Requests => "Requests",
            TrajectoryMode::ModelContext => "Model Context",
            TrajectoryMode::DurableEvents => "Durable Events",
            TrajectoryMode::Recovery => "Recovery",
        };
        let toolbar = div()
            .h(px(38.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                Button::new("trajectory-mode-filter")
                    .ghost()
                    .small()
                    .label(mode_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for (label, mode) in [
                            ("Execution", TrajectoryMode::Execution),
                            ("Requests", TrajectoryMode::Requests),
                            ("Model Context", TrajectoryMode::ModelContext),
                            ("Durable Events", TrajectoryMode::DurableEvents),
                            ("Recovery", TrajectoryMode::Recovery),
                        ] {
                            let view = mode_view.clone();
                            menu =
                                menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_mode = mode;
                                        this.trajectory_category = None;
                                        this.trajectory_lane = None;
                                        this.selected_trajectory_index = None;
                                        cx.notify();
                                    });
                                }));
                        }
                        menu
                    }),
            )
            .child(
                Button::new("trajectory-category-filter")
                    .ghost()
                    .small()
                    .label(category_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let all_view = category_view.clone();
                        let mut menu = menu.item(PopupMenuItem::new("All events").on_click(
                            move |_, _, cx| {
                                all_view.update(cx, |this, cx| {
                                    this.trajectory_category = None;
                                    cx.notify();
                                });
                            },
                        ));
                        for category in categories.iter().cloned() {
                            let selected = category.clone();
                            let view = category_view.clone();
                            menu = menu.item(PopupMenuItem::new(category).on_click(
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_category = Some(selected.clone());
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .children((lanes.len() > 1).then(|| {
                Button::new("trajectory-lane-filter")
                    .ghost()
                    .small()
                    .label(lane_label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let all_view = lane_view.clone();
                        let mut menu =
                            menu.item(PopupMenuItem::new("All lanes").on_click(move |_, _, cx| {
                                all_view.update(cx, |this, cx| {
                                    this.trajectory_lane = None;
                                    cx.notify();
                                });
                            }));
                        for lane in lanes.iter().cloned() {
                            let selected = lane.clone();
                            let view = lane_view.clone();
                            let latest = lane_latest.get(&lane).cloned().unwrap_or_default();
                            menu = menu.item(
                                PopupMenuItem::new(format!("{lane} — {latest}")).on_click(
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.trajectory_lane = Some(selected.clone());
                                            cx.notify();
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    })
            }))
            .child(div().flex_1())
            .child(
                div()
                    .w(px(280.0))
                    .h(px(32.0))
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(Input::new(&self.trajectory_search_input).appearance(false)),
            );
        let tool_count = cache.summary.tool_count;
        let total_dur_ms = cache.summary.total_duration_ms;
        let dur_label = if total_dur_ms < 1000 {
            format!("{total_dur_ms}ms total")
        } else {
            format!("{:.2}s total", total_dur_ms as f64 / 1000.0)
        };
        let anomaly_count = cache.summary.anomaly_count;
        let max_turn = cache.summary.max_turn;

        let stats_bar = div()
            .h(px(26.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_4()
            .px_3()
            .border_b_1()
            .border_color(theme.border.opacity(0.4))
            .bg(theme.muted.opacity(0.15))
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("{max_turn}")),
                    )
                    .child("turns"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(format!("{tool_count}")),
                    )
                    .child("tool calls"),
            )
            .child(
                div().flex().items_center().gap_1().child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(dur_label),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(6.0)).rounded_full().bg(if anomaly_count > 0 {
                        theme.warning
                    } else {
                        theme.success
                    }))
                    .child(format!("{anomaly_count} anomalies")),
            );

        div()
            .id("session-trajectory")
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(overview)
            .child(toolbar)
            .child(stats_bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .id("trajectory-events-container")
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                list(
                                    self.trajectory_list_state.clone(),
                                    cx.processor(Self::render_trajectory_row),
                                )
                                .size_full()
                                .with_sizing_behavior(ListSizingBehavior::Auto),
                            )
                            .child(div().absolute().inset_0().child(
                                gpui_component::scroll::Scrollbar::vertical(
                                    &self.trajectory_list_state,
                                ),
                            )),
                    )
                    .children(inspector),
            )
            .into_any_element()
    }

    fn sync_transcript_rows(
        &mut self,
        messages: Arc<Vec<ChatMessageInfo>>,
        generating: bool,
        session_changed: bool,
    ) {
        if !session_changed
            && Arc::ptr_eq(&messages, &self.transcript_messages)
            && generating == self.transcript_generating
        {
            return;
        }

        let old_message_count = self.transcript_messages.len();
        let old_row_count = self.transcript_rows.len();
        let new_message_count = messages.len();

        if !session_changed
            && new_message_count == old_message_count
            && generating == self.transcript_generating
        {
            let last_changed = messages
                .last()
                .zip(self.transcript_messages.last())
                .is_some_and(|(new, old)| {
                    new.id != old.id
                        || new.content.len() != old.content.len()
                        || new.reasoning_content.as_ref().map(String::len)
                            != old.reasoning_content.as_ref().map(String::len)
                        || new.tool_activities.len() != old.tool_activities.len()
                        || new.streaming != old.streaming
                });
            self.transcript_messages = messages;
            if last_changed {
                self.transcript_list_state
                    .remeasure_items(old_row_count.saturating_sub(1)..old_row_count);
            } else {
                self.transcript_list_state.remeasure();
            }
            return;
        }

        let new_rows = build_transcript_rows(&messages, generating);
        let new_row_count = new_rows.len();
        let working_changed = !session_changed
            && new_message_count == old_message_count
            && generating != self.transcript_generating;
        let prepended = !session_changed
            && new_message_count > old_message_count
            && self
                .transcript_messages
                .first()
                .zip(messages.get(new_message_count - old_message_count))
                .is_some_and(|(old, new)| old.id == new.id)
            && self
                .transcript_messages
                .last()
                .zip(messages.last())
                .is_some_and(|(old, new)| old.id == new.id)
            && new_row_count >= old_row_count;
        let appended = !session_changed
            && new_message_count > old_message_count
            && self
                .transcript_messages
                .first()
                .zip(messages.first())
                .is_some_and(|(old, new)| old.id == new.id)
            && self
                .transcript_messages
                .last()
                .zip(messages.get(old_message_count.saturating_sub(1)))
                .is_some_and(|(old, new)| old.id == new.id)
            && new_row_count >= old_row_count;

        self.transcript_messages = messages;
        self.transcript_rows = new_rows;
        self.transcript_generating = generating;
        if working_changed && generating {
            self.transcript_list_state
                .splice(old_row_count..old_row_count, 1);
        } else if working_changed {
            self.transcript_list_state
                .splice(new_row_count..old_row_count, 0);
        } else if prepended {
            self.transcript_list_state
                .splice(0..0, new_row_count - old_row_count);
        } else if appended {
            self.transcript_list_state
                .splice(old_row_count..old_row_count, new_row_count - old_row_count);
        } else {
            self.transcript_list_state.reset(new_row_count);
        }
        if session_changed {
            self.transcript_list_state.set_follow_mode(FollowMode::Tail);
        }
    }

    fn render_transcript_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let messages = Arc::clone(&self.transcript_messages);
        let content = match self.transcript_rows.get(index).cloned() {
            Some(TranscriptRow::Message(message_index)) => messages
                .get(message_index)
                .map(|message| self.render_message(message, cx)),
            Some(TranscriptRow::Activities(range)) => messages
                .get(range)
                .map(|messages| self.render_activity_group(messages, cx)),
            Some(TranscriptRow::Working) => Some(self.render_working_indicator(cx)),
            None => None,
        };

        div()
            .w_full()
            .max_w(px(CHAT_CONTENT_MAX_WIDTH))
            .mx_auto()
            .children(content)
            .into_any_element()
    }

    fn markdown_state(
        &mut self,
        key: String,
        source: &str,
        cx: &mut Context<Self>,
    ) -> Entity<TextViewState> {
        let key = (self.markdown_cache_namespace.clone(), key);
        let entry = self
            .markdown_states
            .entry(key)
            .or_insert_with(|| MarkdownRenderState {
                source: source.to_owned(),
                state: cx.new(|cx| TextViewState::markdown(source, cx)),
            });

        match classify_markdown_update(&entry.source, source) {
            MarkdownUpdate::Unchanged => {}
            MarkdownUpdate::Append(suffix) => {
                entry.source.push_str(suffix);
                entry
                    .state
                    .update(cx, |state, cx| state.push_str(suffix, cx));
            }
            MarkdownUpdate::Replace => {
                entry.source.clear();
                entry.source.push_str(source);
                entry
                    .state
                    .update(cx, |state, cx| state.set_text(source, cx));
            }
        }

        entry.state.clone()
    }

    fn chat_markdown_view(&self, state: &Entity<TextViewState>) -> TextView {
        let model = self.model.clone();
        TextView::new(state)
            .selectable(true)
            .on_link_click(move |url, event, _window, cx| {
                let activate = match event {
                    ClickEvent::Mouse(click) => {
                        matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
                    }
                    ClickEvent::Keyboard(_) => true,
                    ClickEvent::Touch(click) => !click.long_press,
                };
                if !activate {
                    return;
                }

                match classify_chat_link(url) {
                    ChatLinkTarget::Web => cx.open_url(url),
                    ChatLinkTarget::ProjectFile(path) => {
                        model.update(cx, |state, cx| {
                            state.request_open_file(path);
                            cx.notify();
                        });
                    }
                    ChatLinkTarget::Rejected => {}
                }
            })
    }

    fn render_interactive_code_block(
        &mut self,
        msg_id: &str,
        block_index: usize,
        language: &str,
        header_path: Option<&str>,
        code: &str,
        streaming: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();
        let code_str = code.to_string();
        let copy_code = code.to_string();
        let is_runnable = !streaming
            && is_terminal_runnable_language(language)
            && active_shell_supports_language(language);
        let path_opt = header_path.and_then(|path| match classify_chat_link(path) {
            ChatLinkTarget::ProjectFile(path) => Some(path),
            _ => None,
        });
        let path_for_open = (!streaming).then(|| path_opt.clone()).flatten();

        let display_lang = if language.trim().is_empty() {
            "code"
        } else {
            language.trim()
        };

        div()
            .w_full()
            .my_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_1p5()
                    .bg(theme.secondary)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                Tag::new()
                                    .child(display_lang.to_string())
                                    .with_variant(TagVariant::Secondary)
                                    .small(),
                            )
                            .children(path_opt.map(|path| {
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(path)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(is_runnable.then(|| {
                                let cmd = code_str.clone();
                                let model = model.clone();
                                Button::new(SharedString::from(format!(
                                    "run-term-{msg_id}-{block_index}"
                                )))
                                .icon(IconName::SquareTerminal)
                                .label("Run in Terminal")
                                .xsmall()
                                .secondary()
                                .tooltip("Run in active project terminal")
                                .on_click(move |_event, _window, cx| {
                                    let cmd = normalize_terminal_command(&cmd);
                                    if cmd.lines().filter(|line| !line.trim().is_empty()).count() > 1 {
                                        let model = model.clone();
                                        cx.spawn(async move |cx| {
                                            let result = rfd::AsyncMessageDialog::new()
                                                .set_title("Run multiple terminal commands?")
                                                .set_description("This code block contains multiple commands. Run them in the active terminal?")
                                                .set_buttons(rfd::MessageButtons::YesNo)
                                                .show()
                                                .await;
                                            if matches!(result, rfd::MessageDialogResult::Yes) {
                                                let _ = model.update(cx, |state, cx| {
                                                    controller::dispatch(state, AppAction::RunTerminalCommand(cmd));
                                                    cx.notify();
                                                });
                                            }
                                        }).detach();
                                    } else {
                                        model.update(cx, |state, cx| {
                                            controller::dispatch(state, AppAction::RunTerminalCommand(cmd));
                                            cx.notify();
                                        });
                                    }
                                })
                            }))
                            .children(path_for_open.map(|path| {
                                let model = model.clone();
                                Button::new(SharedString::from(format!(
                                    "open-edit-{msg_id}-{block_index}"
                                )))
                                .icon(IconName::File)
                                .label("Open in Editor")
                                .xsmall()
                                .ghost()
                                .tooltip("Open file in central editor")
                                .on_click(move |_event, _window, cx| {
                                    let path = path.clone();
                                    model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::OpenFileInEditor(path),
                                        );
                                        cx.notify();
                                    });
                                })
                            }))
                            .when(!streaming, |actions| {
                                actions.child(
                                    Button::new(SharedString::from(format!(
                                        "copy-code-{msg_id}-{block_index}"
                                    )))
                                    .icon(IconName::Copy)
                                    .xsmall()
                                    .ghost()
                                    .tooltip("Copy code to clipboard")
                                    .on_click(move |_event, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_code.clone(),
                                        ));
                                        window.push_notification(
                                            Notification::info("Code copied to clipboard"),
                                            cx,
                                        );
                                    }),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .overflow_x_scrollbar()
                    .p_3()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(
                        TextView::markdown(
                            format!("code-{msg_id}-{block_index}"),
                            format!("```{language}\n{}\n```", code.trim_end()),
                        )
                        .selectable(true),
                    ),
            )
    }

    fn render_reasoning_block(
        &mut self,
        msg: &ChatMessageInfo,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let reasoning = msg.reasoning_content.as_deref()?;
        if reasoning.trim().is_empty() {
            return None;
        }
        let theme = cx.theme().colors;
        let is_streaming = msg.streaming;
        let is_expanded = msg.reasoning_expanded;
        let model = self.model.clone();
        let msg_id = msg.id.clone();

        let icon_element = div()
            .text_xs()
            .text_color(if is_streaming {
                theme.primary
            } else {
                theme.muted_foreground
            })
            .child("✦")
            .into_any_element();

        let header = div()
            .id(SharedString::from(format!("reasoning-toggle-{}", msg.id)))
            .h(px(28.0))
            .px_1()
            .rounded_md()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover(|s| s.bg(theme.muted))
            .child(
                div()
                    .w(px(18.0))
                    .flex_none()
                    .text_center()
                    .child(icon_element),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if is_streaming {
                        "Thinking…"
                    } else {
                        "Thought process"
                    }),
            )
            .child(
                Icon::new(if is_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(theme.muted_foreground),
            )
            .on_click(move |_event, _window, cx| {
                model.update(cx, |state, cx| {
                    controller::dispatch(state, AppAction::ToggleReasoningExpanded(msg_id.clone()));
                    cx.notify();
                });
            });

        let detail = is_expanded.then(|| {
            let container = div()
                .ml(px(26.0))
                .mt_1()
                .p_2()
                .max_h(px(300.0))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .text_xs()
                .text_color(theme.muted_foreground)
                .overflow_y_scrollbar();
            if is_streaming {
                container.child(reasoning.to_owned()).into_any_element()
            } else {
                let markdown_state =
                    self.markdown_state(format!("reasoning-{}", msg.id), reasoning, cx);
                container
                    .child(self.chat_markdown_view(&markdown_state))
                    .into_any_element()
            }
        });

        Some(
            Collapsible::new()
                .open(is_expanded)
                .child(header)
                .when_some(detail, |c, content| c.content(content))
                .into_any_element(),
        )
    }

    fn render_message(&mut self, msg: &ChatMessageInfo, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        match msg.role {
            MessageRole::User => {
                let is_queued =
                    msg.id.starts_with("queued-user-") && self.model.read(cx).is_generating;
                let is_steered =
                    msg.id.starts_with("steered-user-") && self.model.read(cx).is_generating;
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_end()
                    .my_2()
                    .px_4()
                    .when(is_queued, |el| {
                        el.child(
                            div().flex().items_center().gap_1().mb_1().child(
                                Tag::new()
                                    .child("Queued for next turn")
                                    .with_variant(TagVariant::Secondary)
                                    .small(),
                            ),
                        )
                    })
                    .when(is_steered, |el| {
                        el.child(
                            div().child(
                                Tag::new()
                                    .child("Steering current turn")
                                    .with_variant(TagVariant::Primary)
                                    .small(),
                            ),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(USER_BUBBLE_MAX_WIDTH))
                            .p_3()
                            .rounded_lg()
                            .bg(theme.secondary)
                            .text_sm()
                            .text_color(theme.secondary_foreground)
                            .child({
                                let markdown_state =
                                    self.markdown_state(msg.id.clone(), &msg.content, cx);
                                self.chat_markdown_view(&markdown_state)
                            })
                            .context_menu({
                                let content = msg.content.clone();
                                move |menu, _window, _cx| {
                                    let text = content.clone();
                                    menu.item(PopupMenuItem::new("Copy Message").on_click(
                                        move |_event, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                            window.push_notification(
                                                Notification::info("Copied to clipboard"),
                                                cx,
                                            );
                                        },
                                    ))
                                }
                            }),
                    )
            }
            MessageRole::Assistant => {
                let reasoning_element = self.render_reasoning_block(msg, cx);
                let tool_elements: Vec<_> = msg
                    .tool_activities
                    .iter()
                    .filter(|tool| tool.title != "update_plan")
                    .map(|tool| self.render_tool_activity(tool, cx))
                    .collect();

                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .my_2()
                    .px_4()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(reasoning_element)
                            .children(if !msg.content.is_empty() {
                                let segments = extract_markdown_segments(&msg.content);
                                let rendered_segments: Vec<AnyElement> = segments
                                    .into_iter()
                                    .enumerate()
                                    .map(|(idx, seg)| match seg {
                                        MarkdownSegment::Markdown(text) => {
                                            if msg.streaming {
                                                div()
                                                    .w_full()
                                                    .text_sm()
                                                    .text_color(theme.foreground)
                                                    .child(text)
                                                    .into_any_element()
                                            } else {
                                                let markdown_state = self.markdown_state(
                                                    format!("{}-seg-{}", msg.id, idx),
                                                    &text,
                                                    cx,
                                                );
                                                self.chat_markdown_view(&markdown_state)
                                                    .into_any_element()
                                            }
                                        }
                                        MarkdownSegment::CodeBlock {
                                            language,
                                            header_path,
                                            code,
                                        } => self
                                            .render_interactive_code_block(
                                                &msg.id,
                                                idx,
                                                &language,
                                                header_path.as_deref(),
                                                &code,
                                                msg.streaming,
                                                cx,
                                            )
                                            .into_any_element(),
                                    })
                                    .collect();

                                Some(
                                    div()
                                        .w_full()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .children(rendered_segments),
                                )
                            } else {
                                None
                            })
                            .children(tool_elements)
                            .context_menu({
                                let content = msg.content.clone();
                                move |menu, _window, _cx| {
                                    let text = content.clone();
                                    menu.item(PopupMenuItem::new("Copy Message").on_click(
                                        move |_event, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                            window.push_notification(
                                                Notification::info("Copied to clipboard"),
                                                cx,
                                            );
                                        },
                                    ))
                                }
                            }),
                    )
            }
            MessageRole::ContextMarker => {
                div().w_full().flex().justify_center().my_2().px_4().child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(msg.content.clone()),
                )
            }
            MessageRole::System => div().flex().justify_center().my_2().child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(msg.content.clone())
                    .context_menu({
                        let content = msg.content.clone();
                        move |menu, _window, _cx| {
                            let text = content.clone();
                            menu.item(PopupMenuItem::new("Copy Message").on_click(
                                move |_event, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                    window.push_notification(
                                        Notification::info("Copied to clipboard"),
                                        cx,
                                    );
                                },
                            ))
                        }
                    }),
            ),
            MessageRole::Error => div().flex().justify_center().my_2().px_4().child(
                div()
                    .w_full()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.danger)
                    .border_1()
                    .border_color(theme.danger)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.danger_foreground)
                                    .child("ERROR"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.danger_foreground)
                                    .child(msg.content.clone())
                                    .context_menu({
                                        let content = msg.content.clone();
                                        move |menu, _window, _cx| {
                                            let text = content.clone();
                                            menu.item(PopupMenuItem::new("Copy Message").on_click(
                                                move |_event, window, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(text.clone()),
                                                    );
                                                    window.push_notification(
                                                        Notification::info("Copied to clipboard"),
                                                        cx,
                                                    );
                                                },
                                            ))
                                        }
                                    }),
                            ),
                    ),
            ),
        }
        .into_any_element()
    }

    fn render_new_task(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let (projects, active_work_dir) = {
            let state = self.model.read(cx);
            (
                state
                    .projects
                    .iter()
                    .map(|project| (project.name.clone(), project.work_dir.clone()))
                    .collect::<Vec<_>>(),
                state.active_work_dir.clone(),
            )
        };
        let selected_project = projects
            .iter()
            .find(|(_, work_dir)| active_work_dir.as_ref() == Some(work_dir))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "Choose a project".to_string());
        let model = self.model.clone();

        let project_picker = Button::new("new-task-project-picker")
            .icon(IconName::Folder)
            .label(selected_project)
            .dropdown_caret(true)
            .ghost()
            .small()
            .dropdown_menu(move |menu, _window, _cx| {
                let mut menu = menu;
                for (name, work_dir) in projects.clone() {
                    let model = model.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(
                        move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectDraftProject(work_dir.clone()),
                                );
                                cx.notify();
                            });
                        },
                    ));
                }

                let model = model.clone();
                menu.separator()
                    .item(PopupMenuItem::new("New project...").on_click(
                        move |_event, _window, cx| {
                            let model = model.clone();
                            cx.spawn(async move |cx| {
                                let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await
                                else {
                                    return;
                                };
                                let path = folder.path().to_path_buf();
                                let _ = model.update(cx, |state, cx| {
                                    controller::dispatch(state, AppAction::AttachProject(path));
                                    cx.notify();
                                });
                            })
                            .detach();
                        },
                    ))
            });

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .pb(px(64.0))
            .child(
                div()
                    .text_2xl()
                    .text_color(theme.primary)
                    .child(IconName::Asterisk),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("What should we build in")
                    .child(project_picker)
                    .child("?"),
            )
            .into_any_element()
    }

    fn resolve_pending_permission(
        &mut self,
        request_id: &str,
        decision: threadlane_session::PermissionDecision,
        cx: &mut Context<Self>,
    ) {
        self.permission_details_open = false;
        self.model.update(cx, |state, cx| {
            state.resolve_active_permission(request_id, decision);
            cx.notify();
        });
        cx.notify();
    }

    fn complete_slash_command(
        &mut self,
        command_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = format!("/{command_name} ");
        self.input_state.update(cx, |state, cx| {
            state.set_value(&value, window, cx);
            let cursor = value.len();
            state.set_selected_range(cursor..cursor, cx);
        });
        self.selected_slash_index = 0;
        self.slash_scroll_handle.scroll_to_item(0);
        cx.notify();
    }

    fn matching_slash_command_count(&mut self, cx: &mut Context<Self>) -> usize {
        if self.dismiss_slash_menu {
            return 0;
        }
        let text = self.input_state.read(cx).value().to_string();
        let project_root = self.model.read(cx).active_work_dir.clone();
        let Some(query) = active_slash_command_query(&text) else {
            return 0;
        };
        self.cached_slash_commands(project_root.as_deref())
            .into_iter()
            .filter(|command| query.is_empty() || command.name.starts_with(query))
            .count()
    }

    fn select_previous_slash_command_action(
        &mut self,
        _: &SelectPreviousSlashCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.matching_slash_command_count(cx);
        if count == 0 {
            return;
        }
        self.selected_slash_index = if self.selected_slash_index == 0 {
            count - 1
        } else {
            self.selected_slash_index - 1
        };
        self.slash_scroll_handle
            .scroll_to_item(self.selected_slash_index);
        cx.notify();
    }

    fn select_next_slash_command_action(
        &mut self,
        _: &SelectNextSlashCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.matching_slash_command_count(cx);
        if count == 0 {
            return;
        }
        self.selected_slash_index = (self.selected_slash_index + 1) % count;
        self.slash_scroll_handle
            .scroll_to_item(self.selected_slash_index);
        cx.notify();
    }

    fn complete_slash_command_action(
        &mut self,
        _: &CompleteSlashCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dismiss_slash_menu {
            return;
        }
        let text = self.input_state.read(cx).value().to_string();
        let project_root = self.model.read(cx).active_work_dir.clone();
        let Some(query) = active_slash_command_query(&text) else {
            return;
        };
        let matching = self
            .cached_slash_commands(project_root.as_deref())
            .into_iter()
            .filter(|command| query.is_empty() || command.name.starts_with(query))
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return;
        }
        let selected = self
            .selected_slash_index
            .min(matching.len().saturating_sub(1));
        self.complete_slash_command(&matching[selected].name, window, cx);
    }

    fn dismiss_slash_command_action(
        &mut self,
        _: &DismissSlashCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_slash_menu = true;
        cx.notify();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();

        if self.permission_details_open {
            if key == "escape" {
                self.permission_details_open = false;
                cx.stop_propagation();
                cx.notify();
                return;
            }

            let request = {
                let state = self.model.read(cx);
                state
                    .active_session_id
                    .as_ref()
                    .and_then(|session_id| state.pending_permissions.get(session_id))
                    .cloned()
            };
            if let Some(request) = request {
                let decision = match key {
                    "y" | "Y" | "enter" => Some(threadlane_session::PermissionDecision::AllowOnce),
                    "a" | "A" => Some(threadlane_session::PermissionDecision::AllowAlways),
                    "n" | "N" => Some(threadlane_session::PermissionDecision::Deny),
                    _ => None,
                };
                if let Some(decision) = decision {
                    self.resolve_pending_permission(&request.id, decision, cx);
                    cx.stop_propagation();
                    return;
                }
            } else {
                self.permission_details_open = false;
                cx.notify();
            }
        }

        let text = self.input_state.read(cx).value().to_string();
        let project_root = self.model.read(cx).active_work_dir.clone();
        if let Some(query) = active_slash_command_query(&text) {
            if !self.dismiss_slash_menu {
                if key == "escape" {
                    self.dismiss_slash_menu = true;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                let matching = self
                    .cached_slash_commands(project_root.as_deref())
                    .into_iter()
                    .filter(|cmd| query.is_empty() || cmd.name.starts_with(query))
                    .collect::<Vec<_>>();
                if !matching.is_empty() {
                    let total = matching.len();
                    match key {
                        "down" => {
                            self.selected_slash_index = (self.selected_slash_index + 1) % total;
                            self.slash_scroll_handle
                                .scroll_to_item(self.selected_slash_index);
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        "up" => {
                            self.selected_slash_index = if self.selected_slash_index == 0 {
                                total.saturating_sub(1)
                            } else {
                                self.selected_slash_index - 1
                            };
                            self.slash_scroll_handle
                                .scroll_to_item(self.selected_slash_index);
                            cx.stop_propagation();
                            cx.notify();
                            return;
                        }
                        "tab" => {
                            let selected = self
                                .selected_slash_index
                                .min(matching.len().saturating_sub(1));
                            let command_name = matching[selected].name.clone();
                            self.complete_slash_command(&command_name, window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn render_permission_details_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let session_id = state.active_session_id.as_ref()?;
        let request = state.pending_permissions.get(session_id)?.clone();
        let theme = cx.theme().colors;

        let action_button = |id: &'static str,
                             label: &'static str,
                             decision: threadlane_session::PermissionDecision,
                             primary: bool,
                             danger: bool| {
            let request_id = request.id.clone();
            Button::new(id)
                .label(label)
                .small()
                .when(primary, |button| button.primary())
                .when(danger, |button| button.danger())
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.resolve_pending_permission(&request_id, decision, cx);
                }))
        };

        Some(
            div()
                .id("permission-details-backdrop")
                .absolute()
                .inset_0()
                .bg(hsla(0.0, 0.0, 0.0, 0.6))
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _event, _window, cx| {
                        this.permission_details_open = false;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("permission-details-modal")
                        .w(px(640.0))
                        .max_w(px(CHAT_CONTENT_MAX_WIDTH))
                        .p_5()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.title_bar)
                        .shadow_xl()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_base()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(theme.foreground)
                                                .child(format!("Permission Request: {}", request.title)),
                                        )
                                        .child(
                                            Tag::new()
                                                .child(request.capability.clone())
                                                .with_variant(TagVariant::Secondary)
                                                .small(),
                                        ),
                                )
                                .child(
                                    Button::new("close-permission-details-dialog-btn")
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.permission_details_open = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .max_h(px(320.0))
                                .p_3()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.background)
                                .text_xs()
                                .text_color(theme.foreground)
                                .overflow_y_scrollbar()
                                .child(request.detail.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Shortcuts: [Y] Allow once · [A] Always · [N] Deny · [Esc] Close"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(action_button(
                                            "details-deny",
                                            "Deny [N]",
                                            threadlane_session::PermissionDecision::Deny,
                                            false,
                                            true,
                                        ))
                                        .child(action_button(
                                            "details-allow-once",
                                            "Allow once [Y]",
                                            threadlane_session::PermissionDecision::AllowOnce,
                                            true,
                                            false,
                                        ))
                                        .child(action_button(
                                            "details-allow-always",
                                            "Always [A]",
                                            threadlane_session::PermissionDecision::AllowAlways,
                                            false,
                                            false,
                                        )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_permission_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx);
        let session_id = state.active_session_id.as_ref()?;
        let request = state.pending_permissions.get(session_id)?.clone();
        let theme = cx.theme().colors;

        let action_button = |id: &'static str,
                             label: &'static str,
                             decision: threadlane_session::PermissionDecision,
                             primary: bool,
                             danger: bool| {
            let request_id = request.id.clone();
            Button::new(id)
                .label(label)
                .xsmall()
                .when(primary, |button| button.primary())
                .when(danger, |button| button.danger())
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.resolve_pending_permission(&request_id, decision, cx);
                }))
        };

        Some(
            div()
                .w_full()
                .flex_none()
                .px_4()
                .pt_1()
                .bg(theme.background)
                .child(
                    div()
                        .w_full()
                        .max_w(px(1000.0))
                        .mx_auto()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.title_bar)
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .flex_none()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child(request.title),
                                )
                                .child(
                                    div()
                                        .id("permission-prompt-detail-text")
                                        .min_w_0()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.permission_details_open = true;
                                            cx.notify();
                                        }))
                                        .child(request.detail),
                                ),
                        )
                        .child(
                            Button::new("permission-details-btn")
                                .icon(IconName::Maximize)
                                .label("Details")
                                .ghost()
                                .xsmall()
                                .tooltip("View full command & arguments")
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.permission_details_open = true;
                                    cx.notify();
                                })),
                        )
                        .child(action_button(
                            "permission-deny",
                            "Deny [N]",
                            threadlane_session::PermissionDecision::Deny,
                            false,
                            true,
                        ))
                        .child(action_button(
                            "permission-allow-once",
                            "Allow once [Y]",
                            threadlane_session::PermissionDecision::AllowOnce,
                            true,
                            false,
                        ))
                        .child(action_button(
                            "permission-allow-always",
                            "Always [A]",
                            threadlane_session::PermissionDecision::AllowAlways,
                            false,
                            false,
                        )),
                )
                .into_any_element(),
        )
    }

    /// Cached slash-command discovery. `available_slash_commands` scans
    /// extension directories and compiles each installed WASM module just to
    /// read its manifest, so it must not run per keystroke in render. The
    /// cache is keyed by project root and refreshed at most once per TTL
    /// while the command menu is open.
    fn cached_slash_commands(
        &mut self,
        project_root: Option<&std::path::Path>,
    ) -> Vec<SlashCommandInfo> {
        const SLASH_COMMAND_CACHE_TTL: Duration = Duration::from_secs(10);
        let project_root = project_root.map(std::path::Path::to_path_buf);
        if let Some((root, loaded_at, commands)) = &self.slash_command_cache {
            if *root == project_root && loaded_at.elapsed() < SLASH_COMMAND_CACHE_TTL {
                return commands.clone();
            }
        }
        let commands = available_slash_commands(project_root.as_deref());
        self.slash_command_cache =
            Some((project_root, std::time::Instant::now(), commands.clone()));
        commands
    }

    fn render_subagent_popover(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (count, active_count) = subagent_popover_counts(
            self.model
                .read(cx)
                .active_subagents()
                .iter()
                .map(|item| item.status),
        )?;
        let open = self.subagents_popover_open;
        let toggle_entity = cx.entity();
        let sync_entity = cx.entity();
        let content_entity = cx.entity();
        Some(
            Popover::new("subagents-popover")
                .anchor(Anchor::BottomRight)
                .appearance(false)
                .open(open)
                .on_open_change(move |open, _window, cx| {
                    sync_entity.update(cx, |this, cx| {
                        this.subagents_popover_open = *open;
                        cx.notify();
                    });
                })
                .trigger(SubagentPopoverTrigger {
                    selected: open,
                    toggle: Toggle::new("subagents-popover-trigger")
                        .ghost()
                        .rounded_full()
                        .tooltip("View subagent activity")
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(Icon::new(IconName::Bot).small())
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(count.to_string()),
                                ),
                        )
                        .on_click(move |open, _window, cx| {
                            toggle_entity.update(cx, |this, cx| {
                                this.subagents_popover_open = *open;
                                cx.notify();
                            });
                        }),
                })
                .content(move |_state, _window, cx| {
                    content_entity.update(cx, |this, cx| {
                        this.render_subagent_popover_content(active_count, cx)
                    })
                })
                .into_any_element(),
        )
    }

    fn render_subagent_popover_content(
        &mut self,
        active_count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let subagents = self.model.read(cx).active_subagents().to_vec();
        let theme = cx.theme().colors;
        let selected_run_id = self
            .selected_subagent_run_id
            .clone()
            .filter(|run_id| {
                subagents
                    .iter()
                    .any(|item| item.journal_run_id.as_deref() == Some(run_id.as_str()))
            })
            .or_else(|| {
                subagents
                    .iter()
                    .find(|item| item.status == SubagentActivityStatus::Running)
                    .or_else(|| subagents.last())
                    .and_then(|item| item.journal_run_id.clone())
            });
        let selected = selected_run_id.as_ref().and_then(|run_id| {
            subagents
                .iter()
                .find(|item| item.journal_run_id.as_deref() == Some(run_id.as_str()))
        });
        let mut rows = Vec::new();
        for (index, item) in subagents.iter().enumerate() {
            let run_id = item
                .journal_run_id
                .clone()
                .unwrap_or_else(|| format!("queued-{}-{}", item.batch_run_id, item.task_index));
            let is_selected = selected_run_id.as_deref() == Some(run_id.as_str());
            let (marker, color, status) = match item.status {
                SubagentActivityStatus::Queued => ("○", theme.muted_foreground, "Queued"),
                SubagentActivityStatus::Running => ("◌", theme.primary, "Working"),
                SubagentActivityStatus::Completed => ("✓", theme.success, "Completed"),
                SubagentActivityStatus::Failed => ("!", theme.danger, "Failed"),
                SubagentActivityStatus::Cancelled => ("×", theme.warning, "Cancelled"),
            };
            let entity = cx.entity();
            rows.push(
                div()
                    .id(SharedString::from(format!("subagent-popup-row-{index}")))
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(is_selected, |row| row.bg(theme.muted))
                    .hover(|row| row.bg(theme.muted))
                    .flex()
                    .items_start()
                    .gap_2()
                    .on_click(move |_event, _window, cx| {
                        entity.update(cx, |this, cx| {
                            this.selected_subagent_run_id = Some(run_id.clone());
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .w(px(18.0))
                            .flex_none()
                            .text_center()
                            .text_color(color)
                            .font_weight(FontWeight::BOLD)
                            .child(marker),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(item.agent.clone()),
                                    )
                                    .child(
                                        div().flex_none().text_xs().text_color(color).child(status),
                                    ),
                            )
                            .children(item.model.as_ref().map(|model| {
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        crate::model_catalog::label_for(model)
                                            .unwrap_or_else(|| model.clone()),
                                    )
                            }))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(if item.task.is_empty() {
                                        item.lane.clone().unwrap_or_default()
                                    } else {
                                        item.task.clone()
                                    }),
                            ),
                    ),
            );
        }
        let detail = selected.map(|item| self.render_subagent_detail(item, cx));
        let count_label = if active_count > 0 {
            format!("{active_count} active")
        } else {
            format!("{} completed", subagents.len())
        };
        div()
            .w(px(520.0))
            .max_w(px(CHAT_CONTENT_MAX_WIDTH - 32.0))
            .max_h(px(520.0))
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Subagents"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(count_label),
                    ),
            )
            .child(
                div()
                    .flex()
                    .min_h(px(240.0))
                    .child(
                        div()
                            .w(px(210.0))
                            .flex_none()
                            .p_2()
                            .border_r_1()
                            .border_color(theme.border)
                            .overflow_y_scrollbar()
                            .children(rows),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .p_3()
                            .overflow_y_scrollbar()
                            .children(detail),
                    ),
            )
            .into_any_element()
    }

    fn render_subagent_detail(
        &mut self,
        item: &SubagentActivityInfo,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let status = match item.status {
            SubagentActivityStatus::Queued => "Queued",
            SubagentActivityStatus::Running => "Working",
            SubagentActivityStatus::Completed => "Completed",
            SubagentActivityStatus::Failed => "Failed",
            SubagentActivityStatus::Cancelled => "Cancelled",
        };
        let messages = item
            .messages
            .iter()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|message| self.render_message(message, cx))
            .collect::<Vec<_>>();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(item.agent.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(status),
                            ),
                    )
                    .when_some(item.model.as_ref(), |header, model| {
                        header.child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted_foreground)
                                .child(
                                    crate::model_catalog::label_for(model)
                                        .unwrap_or_else(|| model.clone()),
                                ),
                        )
                    })
                    .when(!item.task.is_empty(), |header| {
                        header.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(item.task.clone()),
                        )
                    }),
            )
            .children(item.error.as_ref().map(|error| {
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme.danger.opacity(0.08))
                    .text_xs()
                    .text_color(theme.danger)
                    .child(error.clone())
            }))
            .children(messages.is_empty().then(|| {
                div()
                    .py_6()
                    .text_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Waiting for progress…")
            }))
            .children(messages)
            .into_any_element()
    }

    fn render_composer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (
            selected_model,
            reasoning_effort,
            is_generating,
            pending_message,
            active_session_id,
            session_status,
        ) = {
            let state = self.model.read(cx);
            (
                state.selected_model.clone(),
                state.reasoning_effort,
                state.is_generating,
                state.active_pending_composer_message().map(str::to_owned),
                state.active_session_id.clone(),
                state.session_status.clone(),
            )
        };
        let (metrics, context_window) = {
            let state = self.model.read(cx);
            let context_window = state
                .active_context_window()
                .map(|context| ContextMeterContext {
                    current_tokens: context.current_tokens,
                    context_limit: context.context_limit,
                    context_limit_is_estimate: context.context_limit_is_estimate,
                    effective_model: context.effective_model.clone(),
                    last_compaction_seq: context.last_compaction_seq,
                    provisional: context.provisional,
                    estimating: context.estimating,
                });
            (state.active_session_metrics(), context_window)
        };
        let subagent_count = self.model.read(cx).active_subagents().len();
        let has_composer_text = !self.input_state.read(cx).value().trim().is_empty();
        let has_prompt =
            !self.input_state.read(cx).value().trim().is_empty() || !self.pasted_images.is_empty();
        let (model_options, selected_option, project_root) = {
            let state = self.model.read(cx);
            let options = state.available_models().to_vec();
            let opt = options.iter().find(|o| o.id == selected_model).cloned();
            let project = state.active_work_dir.clone();
            (options, opt, project)
        };
        let has_models = !model_options.is_empty();
        let needs_provider = !has_models;
        let model_label = selected_option
            .as_ref()
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "Connect a provider".to_string());
        // Selecting an ACP agent picks the *agent*; the agent then picks its own
        // model. Naming it here rather than only in the settings menu is what
        // makes choosing a model visibly take effect, since this is the control
        // a user reads to answer "which model am I on".
        let model_label = match self.model.read(cx).active_acp_model_name() {
            Some(agent_model) => format!("{model_label} · {agent_model}"),
            None => model_label,
        };
        // Selecting an ACP agent picks the *agent*; the agent then runs one of
        // its own models. Both are "which model am I on", so both belong in
        // this one control rather than split across two.
        let acp_model_option = threadlane_session::is_acp_model(&selected_model)
            .then(|| {
                threadlane_session::config_option_for(
                    self.model.read(cx).active_acp_config_options(),
                    threadlane_session::ACP_CONFIG_CATEGORY_MODEL,
                )
                .cloned()
            })
            .flatten();
        let acp_model_menu_model = self.model.clone();
        let model_for_picker = self.model.clone();
        let queue_model = self.model.clone();
        let steer_model = self.model.clone();
        let dismiss_model = self.model.clone();
        let dismiss_input = self.input_state.clone();
        let cancel_model = self.model.clone();
        let queue_prompt_model = self.model.clone();
        let queue_prompt_input = self.input_state.clone();
        let steer_prompt_model = self.model.clone();
        let steer_prompt_input = self.input_state.clone();
        let send_model = self.model.clone();
        let send_input = self.input_state.clone();

        let image_chips = self
            .pasted_images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                let name = image.display_name.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme.secondary)
                    .text_xs()
                    .child("▣")
                    .child(name)
                    .child(
                        Button::new(("remove-pasted-image", index))
                            .icon(IconName::Close)
                            .xsmall()
                            .ghost()
                            .tooltip("Remove image")
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                if index < this.pasted_images.len() {
                                    this.pasted_images.remove(index);
                                    cx.notify();
                                }
                            })),
                    )
            })
            .collect::<Vec<_>>();

        let provider_setup_model = self.model.clone();
        let provider_setup_banner = needs_provider.then(|| {
            div()
                .w_full()
                .max_w(px(1000.0))
                .mx_auto()
                .mb_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.warning.opacity(0.45))
                .bg(theme.warning.opacity(0.08))
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child("Connect a model provider to start"),
                        )
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            "Add an account or API key in Settings, then choose a model here.",
                        )),
                )
                .child(
                    Button::new("composer-open-provider-settings")
                        .icon(IconName::Settings)
                        .label("Open Settings")
                        .small()
                        .primary()
                        .on_click(move |_event, _window, cx| {
                            provider_setup_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::OpenSettings);
                                cx.notify();
                            });
                        }),
                )
        });

        let (
            projects_list,
            active_work_dir,
            is_new_task,
            draft_work_mode,
            active_session_is_worktree,
            active_session_worktree_available,
            active_session_project_name,
        ) = {
            let state = self.model.read(cx);
            let active_dir = state.active_work_dir.clone();
            let active_session = state.active_session_id.as_ref().and_then(|sid| {
                state.projects.iter().find_map(|project| {
                    project
                        .sessions
                        .iter()
                        .find(|session| &session.id == sid)
                        .map(|session| {
                            (
                                project.name.clone(),
                                session.is_worktree,
                                session.worktree_available,
                            )
                        })
                })
            });
            (
                state
                    .projects
                    .iter()
                    .map(|p| (p.name.clone(), p.work_dir.clone()))
                    .collect::<Vec<_>>(),
                active_dir,
                state.is_new_task,
                state.draft_work_mode,
                active_session
                    .as_ref()
                    .is_some_and(|(_, is_worktree, _)| *is_worktree),
                active_session
                    .as_ref()
                    .is_none_or(|(_, _, available)| *available),
                active_session.map(|(project_name, _, _)| project_name),
            )
        };

        let effective_work_mode = if is_new_task {
            draft_work_mode
        } else if active_session_is_worktree {
            WorkMode::Worktree
        } else {
            WorkMode::Local
        };

        let selected_project_name = if is_new_task {
            None
        } else {
            active_session_project_name
        }
        .or_else(|| {
            projects_list
                .iter()
                .find(|(_, work_dir)| active_work_dir.as_ref() == Some(work_dir))
                .map(|(name, _)| name.clone())
        })
        .unwrap_or_else(|| "Select project".to_string());

        let project_chip_model = self.model.clone();
        let project_chip = Button::new("composer-project-chip")
            .icon(IconName::Folder)
            .label(selected_project_name)
            .dropdown_caret(true)
            .ghost()
            .xsmall()
            .dropdown_menu(move |menu, _window, _cx| {
                let mut menu = menu;
                for (name, work_dir) in projects_list.clone() {
                    let model = project_chip_model.clone();
                    menu = menu.item(PopupMenuItem::new(name).on_click(
                        move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectDraftProject(work_dir.clone()),
                                );
                                cx.notify();
                            });
                        },
                    ));
                }

                let model = project_chip_model.clone();
                menu.separator()
                    .item(PopupMenuItem::new("New project...").on_click(
                        move |_event, _window, cx| {
                            let model = model.clone();
                            cx.spawn(async move |cx| {
                                let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await
                                else {
                                    return;
                                };
                                let path = folder.path().to_path_buf();
                                let _ = model.update(cx, |state, cx| {
                                    controller::dispatch(state, AppAction::AttachProject(path));
                                    cx.notify();
                                });
                            })
                            .detach();
                        },
                    ))
            });

        let work_mode_model = self.model.clone();
        let work_mode_label = match effective_work_mode {
            WorkMode::Local => "Local",
            WorkMode::Worktree => {
                if is_new_task {
                    "New local worktree"
                } else if active_session_worktree_available {
                    "Worktree"
                } else {
                    "Worktree unavailable"
                }
            }
        };

        let work_mode_chip = Button::new("composer-workmode-chip")
            .icon(if effective_work_mode == WorkMode::Worktree {
                Icon::default().path("icons/git/branch.svg")
            } else {
                Icon::new(IconName::SquareTerminal)
            })
            .label(work_mode_label)
            .dropdown_caret(true)
            .ghost()
            .xsmall()
            .dropdown_menu(move |menu, _window, _cx| {
                let local_model = work_mode_model.clone();
                let wt_model = work_mode_model.clone();
                menu.item(PopupMenuItem::label("Work in"))
                    .item(
                        PopupMenuItem::new("Local")
                            .icon(IconName::SquareTerminal)
                            .checked(effective_work_mode == WorkMode::Local)
                            .on_click(move |_event, _window, cx| {
                                local_model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectWorkMode(WorkMode::Local),
                                    );
                                    cx.notify();
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("New local worktree")
                            .icon(Icon::default().path("icons/git/branch.svg"))
                            .checked(effective_work_mode == WorkMode::Worktree)
                            .on_click(move |_event, _window, cx| {
                                wt_model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectWorkMode(WorkMode::Worktree),
                                    );
                                    cx.notify();
                                });
                            }),
                    )
            });

        // Keep the context row focused on the two choices that affect where
        // work happens. The branch is already represented by the worktree
        // choice and is not an additional control the user needs to parse.
        let composer_context_bar = div()
            .w_full()
            .max_w(px(1000.0))
            .mx_auto()
            .mb_1p5()
            .flex()
            .items_center()
            .gap_1p5()
            .child(project_chip)
            .child(work_mode_chip);

        let pending_preview = pending_message.map(|text| {
            div()
                .w_full()
                .max_w(px(1000.0))
                .mx_auto()
                .mb_2()
                .h(px(52.0))
                .px_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .flex()
                .items_center()
                .gap_3()
                .child(
                    Tag::new()
                        .child("Pending")
                        .with_variant(TagVariant::Secondary)
                        .small(),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(theme.foreground)
                        .child(text),
                )
                .child(
                    Button::new("queue-pending-message")
                        .icon(IconName::Plus)
                        .xsmall()
                        .secondary()
                        .tooltip("Queue after the current response")
                        .on_click(move |_event, _window, cx| {
                            queue_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::QueuePendingMessage);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("steer-pending-message")
                        .icon(IconName::ArrowRight)
                        .xsmall()
                        .primary()
                        .tooltip("Steer the current response")
                        .on_click(move |_event, _window, cx| {
                            steer_model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::SteerPendingMessage);
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Button::new("dismiss-pending-message")
                        .icon(IconName::Undo2)
                        .xsmall()
                        .ghost()
                        .tooltip("Edit message in the composer")
                        .on_click(move |_event, window, cx| {
                            let restored = dismiss_model.update(cx, |state, cx| {
                                let restored =
                                    state.active_pending_composer_message().map(str::to_owned);
                                controller::dispatch(state, AppAction::DismissPendingMessage);
                                cx.notify();
                                restored
                            });
                            if let Some(restored) = restored {
                                dismiss_input.update(cx, |input, cx| {
                                    input.set_value(restored, window, cx);
                                });
                            }
                        }),
                )
        });

        let model_picker = Button::new("composer-model-picker")
            .label(model_label)
            .dropdown_caret(true)
            .ghost()
            .disabled(!has_models);

        let model_picker = if let Some(option) = selected_option.as_ref() {
            model_picker.icon(Icon::default().path(option.provider.icon_path()))
        } else {
            model_picker
        };
        let model_picker = model_picker.dropdown_menu(move |menu, _window, _cx| {
            let menu = model_options.iter().cloned().fold(menu, |menu, option| {
                let model = model_for_picker.clone();
                menu.item(
                    PopupMenuItem::new(option.label)
                        .icon(Icon::default().path(option.provider.icon_path()))
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectModel(option.id.to_string()),
                                );
                                cx.notify();
                            });
                        }),
                )
            });
            let Some(acp_model) = acp_model_option.as_ref() else {
                return menu;
            };
            let current = acp_model.current_value();
            let config_id = acp_model.id.clone();
            let menu = menu
                .item(PopupMenuItem::separator())
                .item(PopupMenuItem::label(acp_model.name.clone()));
            acp_model.options.iter().fold(menu, |menu, choice| {
                let model = acp_model_menu_model.clone();
                let config_id = config_id.clone();
                let value = choice.value.clone();
                menu.item(
                    PopupMenuItem::new(choice.name.clone())
                        .checked(current == Some(choice.value.as_str()))
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SetAcpConfigOption {
                                        config_id: config_id.clone(),
                                        value: value.clone(),
                                    },
                                );
                                cx.notify();
                            });
                        }),
                )
            })
        });

        let effort_model = self.model.clone();
        let effort_picker = Button::new("composer-reasoning-effort-picker")
            .icon(Icon::default().path("icons/effort.svg"))
            .label(reasoning_effort.label())
            .tooltip(format!("Reasoning effort: {}", reasoning_effort.label()))
            .dropdown_caret(true)
            .ghost()
            .dropdown_menu(move |menu, _window, _cx| {
                [
                    ReasoningEffort::Off,
                    ReasoningEffort::Minimal,
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                    ReasoningEffort::XHigh,
                    ReasoningEffort::Max,
                ]
                .into_iter()
                .fold(menu, |menu, effort| {
                    let model = effort_model.clone();
                    menu.item(
                        PopupMenuItem::new(effort.label())
                            .icon(Icon::default().path("icons/effort.svg"))
                            .checked(effort == reasoning_effort)
                            .on_click(move |_event, _window, cx| {
                                model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::SelectReasoningEffort(effort),
                                    );
                                    cx.notify();
                                });
                            }),
                    )
                })
            });

        let input_value = self.input_state.read(cx).value().to_string();
        let mut slash_completion_active = false;
        let command_menu = if let Some(query) = active_slash_command_query(&input_value) {
            if self.dismiss_slash_menu {
                div().into_any_element()
            } else {
                let commands = self
                    .cached_slash_commands(project_root.as_deref())
                    .into_iter()
                    .filter(|command| query.is_empty() || command.name.starts_with(query))
                    .collect::<Vec<_>>();
                let command_count = commands.len();
                let has_commands = command_count > 0;
                slash_completion_active = has_commands;
                let selected_idx = self.selected_slash_index.min(command_count.saturating_sub(1));
                div()
                    .absolute()
                    .bottom_full()
                    .left(px(0.0))
                    .mb(px(8.0))
                    .w_full()
                    .max_w(px(640.0))
                    .max_h(px(320.0))
                    .flex()
                    .flex_col()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_xl()
                    .p_1p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(26.0))
                            .px_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div().font_weight(FontWeight::SEMIBOLD).child("COMMANDS"),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .child("↑↓ navigate · Tab/Enter select · Esc dismiss"),
                                    ),
                            )
                            .child(if has_commands {
                                format!("{}/{}", selected_idx + 1, command_count)
                            } else {
                                "0/0".to_string()
                            }),
                    )
                    .child(
                        div()
                            .id("slash-command-list")
                            .relative()
                            .track_scroll(&self.slash_scroll_handle)
                            .overflow_y_scroll()
                            .vertical_scrollbar(&self.slash_scroll_handle)
                            .max_h(px(260.0))
                            .when(!has_commands, |list| {
                                list.child(
                                    div()
                                        .h(px(36.0))
                                        .flex()
                                        .items_center()
                                        .px_2()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("No matching commands"),
                                )
                            })
                            .children(commands.into_iter().enumerate().map(
                                |(idx, command)| {
                                    let is_active = idx == selected_idx;
                                    let command_name = command.name.clone();
                                    div()
                                        .id(SharedString::from(format!(
                                            "composer-command-{}",
                                            command.name
                                        )))
                                        .h(px(30.0))
                                        .flex()
                                        .items_center()
                                        .rounded_md()
                                        .px_2()
                                        .text_sm()
                                        .bg(if is_active {
                                            theme.accent.opacity(0.16)
                                        } else {
                                            hsla(0.0, 0.0, 0.0, 0.0)
                                        })
                                        .hover(|style| style.bg(theme.list_hover))
                                        .cursor_pointer()
                                        .child(
                                            div()
                                                .w(px(112.0))
                                                .flex_none()
                                                .font_weight(if is_active {
                                                    FontWeight::BOLD
                                                } else {
                                                    FontWeight::SEMIBOLD
                                                })
                                                .text_color(if is_active {
                                                    theme.primary
                                                } else {
                                                    theme.foreground
                                                })
                                                .child(format!("/{}", command.name)),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_color(if is_active {
                                                    theme.foreground
                                                } else {
                                                    theme.muted_foreground
                                                })
                                                .child(command.description),
                                        )
                                        .on_click(cx.listener(move |this, _event, window, cx| {
                                            this.complete_slash_command(&command_name, window, cx);
                                        }))
                                },
                            )),
                    )
                    .into_any_element()
            }
        } else {
            div().into_any_element()
        };

        let meter = context_meter_view_model(
            context_window.as_ref(),
            &ContextMeterMetrics {
                billed_input_tokens: metrics.billed_input_tokens(),
                output_tokens: metrics.output_tokens,
                cache_hit_percent: metrics.cache_hit_percent(),
            },
            !threadlane_session::is_acp_model(&selected_model),
        );
        let displayed_percent = meter.percent.unwrap_or_default();
        let meter_color = if meter.percent.is_none() || displayed_percent == 0.0 {
            theme.muted_foreground
        } else if displayed_percent >= CONTEXT_METER_DANGER_PCT {
            theme.danger
        } else if displayed_percent >= CONTEXT_METER_WARN_PCT {
            theme.warning
        } else {
            theme.accent
        };
        let subagent_popover = self.render_subagent_popover(cx);
        let context_meter_open = self.context_meter_open;
        let toggle_context_meter = cx.entity();
        let sync_context_meter = cx.entity();
        let context_meter = Popover::new("context-window-popover")
            .anchor(Anchor::BottomRight)
            .appearance(false)
            .open(context_meter_open)
            .on_open_change(move |open, _window, cx| {
                sync_context_meter.update(cx, |this, cx| {
                    if this.context_meter_open != *open {
                        this.context_meter_open = *open;
                        cx.notify();
                    }
                });
            })
            .trigger(ContextMeterTrigger {
                selected: context_meter_open,
                toggle: Toggle::new("context-meter-badge")
                    .ghost()
                    .rounded_full()
                    .size(px(32.0))
                    .tooltip(meter.detail_label.clone())
                    .child(
                        ProgressCircle::new("context-meter-circle")
                            .value(meter.bar_percent as f32)
                            .color(meter_color)
                            .size(px(24.0)),
                    )
                    .on_click(move |open, _window, cx| {
                        toggle_context_meter.update(cx, |this, cx| {
                            this.context_meter_open = *open;
                            cx.notify();
                        });
                    }),
            })
            .content(move |_state, _window, _cx| {
                let bar_width = meter.bar_percent / 100.0 * 308.0;
                let current_summary = match meter.percent {
                    Some(percent) => format!(
                        "{percent:.0}% · {}{}",
                        meter.current_label,
                        if meter.provisional {
                            " · provisional"
                        } else {
                            ""
                        }
                    ),
                    None => meter.current_label.clone(),
                };
                div()
                    .w(px(340.0))
                    .p_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("Current context"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(current_summary),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(5.0))
                            .rounded_full()
                            .bg(theme.muted.opacity(0.8))
                            .child(
                                div()
                                    .h_full()
                                    .w(px(bar_width as f32))
                                    .rounded_full()
                                    .bg(meter_color),
                            ),
                    )
                    .when_some(meter.effective_model.clone(), |card, effective_model| {
                        card.child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Model")
                                .child(effective_model),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Total processed")
                            .child(meter.total_processed_label.clone()),
                    )
                    .when_some(meter.cache_hit_label.clone(), |card, cache_hit| {
                        card.child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Cache hit")
                                .child(cache_hit),
                        )
                    })
                    .when_some(meter.last_compaction_seq, |card, sequence| {
                        card.child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Last compacted")
                                .child(format!("Record #{sequence}")),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Context is compacted automatically when needed."),
                    )
            });

        let stashed_draft = active_session_id
            .as_ref()
            .and_then(|id| self.model.read(cx).get_stashed_prompt(id).cloned());
        let stash_model = self.model.clone();
        let stash_input = self.input_state.clone();
        let stash_session_id = active_session_id.clone();
        let stash_banner = stashed_draft.map(|draft| {
            let restore_input = stash_input.clone();
            let restore_model = stash_model.clone();
            let restore_session_id = stash_session_id.clone();
            let dismiss_model = stash_model.clone();
            let dismiss_session_id = stash_session_id.clone();
            let preview_text = if draft.chars().count() > 60 {
                format!("{}…", draft.chars().take(60).collect::<String>())
            } else {
                draft.clone()
            };
            div()
                .w_full()
                .mb_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.accent.opacity(0.3))
                .bg(theme.accent.opacity(0.1))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .child(IconName::File)
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.foreground)
                                .truncate()
                                .child(format!("Stashed draft: \"{preview_text}\"")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Button::new("restore-stashed-draft")
                                .label("Restore Draft")
                                .small()
                                .primary()
                                .on_click(move |_event, window, cx| {
                                    if let Some(session_id) = &restore_session_id {
                                        if let Some(text) = restore_model.update(cx, |state, cx| {
                                            let popped = state.pop_stashed_prompt(session_id);
                                            cx.notify();
                                            popped
                                        }) {
                                            restore_input.update(cx, |input, cx| {
                                                input.set_value(text, window, cx);
                                            });
                                        }
                                    }
                                }),
                        )
                        .child(
                            Button::new("dismiss-stashed-draft")
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip("Discard stash")
                                .on_click(move |_event, _window, cx| {
                                    if let Some(session_id) = &dismiss_session_id {
                                        dismiss_model.update(cx, |state, cx| {
                                            state.clear_stashed_prompt(session_id);
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                )
        });

        let stash_button = {
            let do_stash_input = self.input_state.clone();
            let do_stash_model = self.model.clone();
            let do_stash_session_id = active_session_id.clone();
            Button::new("stash-prompt-btn")
                .icon(IconName::Folder)
                .tooltip("Stash draft")
                .ghost()
                .small()
                .disabled(is_generating || !has_prompt)
                .on_click(move |_event, window, cx| {
                    if let Some(session_id) = &do_stash_session_id {
                        let text = do_stash_input.read(cx).value().to_string();
                        if !text.trim().is_empty() {
                            do_stash_model.update(cx, |state, cx| {
                                state.stash_prompt(session_id, text);
                                cx.notify();
                            });
                            do_stash_input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                            });
                        }
                    }
                })
        };

        let billed_input_tokens = metrics.billed_input_tokens();
        let cache_hit = metrics
            .cache_hit_percent()
            .map(|percent| format!(" · Cache hit {percent}%"))
            .unwrap_or_default();

        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .px_4()
            .pt_3()
            .pb_2()
            .bg(theme.background)
            .children(provider_setup_banner)
            .children(session_status.filter(|status| {
                !status.trim().is_empty()
                    && status != "Working…"
                    && status != "Reconciling session…"
            }).map(|status| {
                let is_error = status.starts_with("Could not")
                    || status.starts_with("Failed")
                    || status.starts_with("Error");
                div()
                    .w_full()
                    .max_w(px(1000.0))
                    .mx_auto()
                    .mb_2()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(if is_error {
                        theme.danger.opacity(0.4)
                    } else {
                        theme.border
                    })
                    .bg(if is_error {
                        theme.danger.opacity(0.08)
                    } else {
                        theme.title_bar
                    })
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(if is_error {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(if is_error {
                        IconName::CircleX
                    } else {
                        IconName::Asterisk
                    })
                    .child(div().min_w_0().child(status))
            }))
            .children(pending_preview)
            .child(composer_context_bar)
            .child(
                div()
                    .w_full()
                    .max_w(px(1000.0))
                    .mx_auto()
                    .relative()
                    .min_h(px(132.0))
                    .flex()
                    .flex_col()
                    .justify_between()
                    .p_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .on_action(cx.listener(Self::paste_composer_clipboard))
                    .when(slash_completion_active, |composer| {
                        composer
                            .key_context(SLASH_COMMAND_KEY_CONTEXT)
                            .on_action(cx.listener(Self::complete_slash_command_action))
                            .on_action(cx.listener(Self::select_previous_slash_command_action))
                            .on_action(cx.listener(Self::select_next_slash_command_action))
                            .on_action(cx.listener(Self::dismiss_slash_command_action))
                    })
                    .children(stash_banner)
                    .children(
                        (!image_chips.is_empty())
                            .then(|| div().flex().flex_wrap().gap_2().children(image_chips)),
                    )
                    .child(command_menu)
                    .child(
                        Textarea::new(&self.input_state)
                            .appearance(false)
                            .bordered(false),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(model_picker)
                            .child(effort_picker)
                            .child(div().flex_1())
                            .child(stash_button)
                            .children(subagent_popover)
                            .child(context_meter)
                            .children(if is_generating {
                                    vec![
                                        Button::new("composer-queue-btn")
                                            .icon(IconName::Plus)
                                            .label("Queue")
                                            .small()
                                            .secondary()
                                            .disabled(!has_composer_text)
                                            .tooltip("Queue for next turn (Enter)")
                                            .on_click(cx.listener(move |this, _event, window, cx| {
                                                let text = queue_prompt_input.read(cx).value().to_string();
                                                if !text.trim().is_empty() {
                                                    queue_prompt_model.update(cx, |state, cx| {
                                                        let images = std::mem::take(&mut this.pasted_images);
                                                        controller::dispatch(state, AppAction::StageBusyMessage { text, images });
                                                        controller::dispatch(state, AppAction::QueuePendingMessage);
                                                        cx.notify();
                                                    });
                                                    queue_prompt_input.update(cx, |state, cx| {
                                                        state.set_value("", window, cx);
                                                    });
                                                    this.transcript_list_state.scroll_to_end();
                                                    cx.notify();
                                                }
                                            }))
                                            .into_any_element(),
                                        Button::new("composer-steer-btn")
                                            .icon(IconName::ArrowRight)
                                            .label("Steer")
                                            .small()
                                            .primary()
                                            .disabled(!has_composer_text)
                                            .tooltip("Steer current turn immediately (Cmd+Enter)")
                                            .on_click(cx.listener(move |this, _event, window, cx| {
                                                let text = steer_prompt_input.read(cx).value().to_string();
                                                if !text.trim().is_empty() {
                                                    steer_prompt_model.update(cx, |state, cx| {
                                                        let images = std::mem::take(&mut this.pasted_images);
                                                        controller::dispatch(state, AppAction::StageBusyMessage { text, images });
                                                        controller::dispatch(state, AppAction::SteerPendingMessage);
                                                        cx.notify();
                                                    });
                                                    steer_prompt_input.update(cx, |state, cx| {
                                                        state.set_value("", window, cx);
                                                    });
                                                    this.transcript_list_state.scroll_to_end();
                                                    cx.notify();
                                                }
                                            }))
                                            .into_any_element(),
                                        Button::new("composer-stop-btn")
                                            .icon(IconName::CircleX)
                                            .small()
                                            .danger()
                                            .tooltip("Stop generation (Esc)")
                                            .on_click(cx.listener(move |_this, _event, _window, cx| {
                                                cancel_model.update(cx, |state, cx| {
                                                    controller::dispatch(
                                                        state,
                                                        AppAction::CancelGeneration,
                                                    );
                                                    cx.notify();
                                                });
                                            }))
                                            .into_any_element(),
                                    ]
                            } else {
                                vec![
                                    Button::new("send-btn")
                                        .w(px(40.0))
                                        .h(px(40.0))
                                        .icon(IconName::ArrowUp)
                                        .tooltip(if needs_provider {
                                            "Connect a model provider in Settings before sending"
                                        } else if has_prompt {
                                            "Send message (Enter)"
                                        } else {
                                            "Type a message to send"
                                        })
                                        .primary()
                                        .disabled(!has_prompt || needs_provider)
                                        .on_click(cx.listener(move |this, _event, window, cx| {
                                            let text = send_input.read(cx).value().to_string();
                                            if !text.trim().is_empty() || !this.pasted_images.is_empty() {
                                                let images = std::mem::take(&mut this.pasted_images);
                                                send_model.update(cx, |state, cx| {
                                                    controller::dispatch(
                                                        state,
                                                        AppAction::SendPromptWithImages {
                                                            text,
                                                            images,
                                                        },
                                                    );
                                                    cx.notify();
                                                });
                                                send_input.update(cx, |state, cx| {
                                                    state.set_value("", window, cx);
                                                });
                                                this.transcript_list_state.scroll_to_end();
                                                cx.notify();
                                            }
                                        }))
                                        .into_any_element(),
                                ]
                            }),
                    ),
            )
            .when(
                metrics.turns > 0
                    || metrics.tool_calls > 0
                    || billed_input_tokens > 0
                    || metrics.output_tokens > 0
                    || subagent_count > 0,
                |this| {
                    this.child(
                        div()
                            .w_full()
                            .max_w(px(1000.0))
                            .mx_auto()
                            .flex()
                            .justify_center()
                            .pt_1()
                            .pb_2()
                            .px_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} turns · {} tool calls{cache_hit} · {} input / {} output tokens · {} subagents",
                                metrics.turns,
                                metrics.tool_calls,
                                crate::model_catalog::format_tokens(
                                    billed_input_tokens.min(u64::from(u32::MAX)) as u32
                                ),
                                crate::model_catalog::format_tokens(
                                    metrics.output_tokens.min(u64::from(u32::MAX)) as u32
                                ),
                                subagent_count,
                            )),
                    )
                },
            )
    }
}

impl Render for ChatListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (messages, is_new_task, active_plan, session_key, is_generating, has_active_permission) = {
            let state = self.model.read(cx);
            let has_active_permission = state
                .active_session_id
                .as_ref()
                .is_some_and(|session_id| state.pending_permissions.contains_key(session_id));
            (
                state.messages.clone(),
                state.is_new_task,
                state.active_plan.clone(),
                state
                    .active_work_dir
                    .clone()
                    .zip(state.active_session_id.clone()),
                state.is_generating,
                has_active_permission,
            )
        };
        let session_changed = session_key != self.last_session_key;
        if session_changed {
            self.markdown_cache_namespace = session_key
                .as_ref()
                .map(|(work_dir, session_id)| {
                    SharedString::from(format!("{}\0{session_id}", work_dir.display()))
                })
                .unwrap_or_else(|| SharedString::from(""));
            if markdown_cache_exceeded(self.markdown_states.len()) {
                self.markdown_states.clear();
            }
            self.last_session_key = session_key;
            self.initial_scroll_frames = 6;
            self.trajectory_category = None;
            self.trajectory_lane = None;
            self.selected_trajectory_index = None;
            self.trajectory_search.clear();
            self.trajectory_cache = None;
            self.trajectory_raw_json = None;
            self.subagents_popover_open = false;
            self.selected_subagent_run_id = None;
            self.selected_slash_index = 0;
            self.dismiss_slash_menu = false;
            self.permission_details_open = false;
            self.trajectory_search_input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }
        if self.permission_details_open && !has_active_permission {
            self.permission_details_open = false;
        }
        self.sync_transcript_rows(messages.clone(), is_generating, session_changed);
        if let Some(prompt) = self
            .model
            .update(cx, |state, _cx| state.requested_composer_prompt.take())
        {
            self.current_tab = CentralTab::Chat;
            self.input_state.update(cx, |input, cx| {
                input.set_value(&prompt, window, cx);
            });
        }
        if self.initial_scroll_frames > 0 {
            self.transcript_list_state.scroll_to_end();
            self.initial_scroll_frames = self.initial_scroll_frames.saturating_sub(1);
        }
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .bg(theme.background)
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(self.render_header(cx))
            .child(match self.current_tab {
                CentralTab::Editor => self.editor.clone().into_any_element(),
                CentralTab::Trajectory => self.render_trajectory(cx),
                CentralTab::Chat => {
                    if is_new_task {
                        self.render_new_task(cx)
                    } else if messages.is_empty() {
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .px_4()
                            .child(
                                div()
                                    .w_full()
                                    .max_w(px(440.0))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap_3()
                                    .px_6()
                                    .py_8()
                                    .rounded_xl()
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.title_bar)
                                    .child(
                                        div()
                                            .size(px(40.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded_full()
                                            .bg(theme.primary.opacity(0.12))
                                            .text_color(theme.primary)
                                            .child(IconName::Bot),
                                    )
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.foreground)
                                            .child("Ready when you are"),
                                    )
                                    .child(
                                        div()
                                            .max_w(px(320.0))
                                            .text_center()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(
                                                "Describe what you want to build, investigate, or fix. Threadlane can use your project context and tools to help.",
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Press Enter to send · Shift+Enter for a new line"),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .id("chat-transcript-container")
                            .relative()
                            .w_full()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(
                                list(
                                    self.transcript_list_state.clone(),
                                    cx.processor(Self::render_transcript_row),
                                )
                                .w_full()
                                .max_w(px(CHAT_CONTENT_MAX_WIDTH))
                                .h_full()
                                .mx_auto()
                                .pt_3()
                                .pb_6()
                                .with_sizing_behavior(ListSizingBehavior::Auto),
                            )
                            .child(div().absolute().inset_0().child(
                                gpui_component::scroll::Scrollbar::vertical(
                                    &self.transcript_list_state,
                                ),
                            ))
                            .into_any_element()
                    }
                }
            })
            .children(
                (self.current_tab == CentralTab::Chat)
                    .then(|| self.render_plan_tracker(&active_plan, cx))
                    .flatten(),
            )
            .children(
                (self.current_tab == CentralTab::Chat)
                    .then(|| self.render_permission_prompt(cx))
                    .flatten(),
            )
            .children((self.current_tab == CentralTab::Chat).then(|| self.render_composer(cx)))
            .children(
                (self.current_tab == CentralTab::Chat && self.permission_details_open)
                    .then(|| self.render_permission_details_dialog(cx))
                    .flatten(),
            )
    }
}

#[cfg(test)]
mod hot_path_tests {
    use super::{
        active_slash_command_query, build_trajectory_rows, build_transcript_rows,
        classify_chat_link, classify_markdown_update, contains_case_insensitive,
        context_meter_view_model, extend_trajectory_facets, extend_trajectory_previews,
        extend_trajectory_rows, extend_trajectory_summary, extract_markdown_segments,
        format_trajectory_raw_json, grouped_tool_activities, is_terminal_runnable_language,
        markdown_cache_exceeded, next_chat_stream_batch, normalize_terminal_command,
        reconcile_trajectory_entries, reconcile_trajectory_entries_by_epoch,
        subagent_popover_counts, summarize_trajectory, ChatLinkTarget, ContextMeterContext,
        ContextMeterMetrics, MarkdownSegment, MarkdownUpdate, TrajectoryCacheKey, TrajectoryMode,
        TrajectoryRow, TranscriptRow, editor_target_matches_active_work_dir, INPUT_KEY_CONTEXT, MARKDOWN_CACHE_ENTRY_LIMIT,
        SLASH_COMMAND_BINDING_CONTEXT, SLASH_COMMAND_KEY_CONTEXT,
    };

    #[test]
    fn editor_targets_only_open_for_the_active_git_checkout() {
        let worktree = std::path::Path::new("/projects/app/.threadlane/worktrees/session");
        let canonical = std::path::Path::new("/projects/app");

        assert!(editor_target_matches_active_work_dir(worktree, Some(worktree)));
        assert!(!editor_target_matches_active_work_dir(worktree, Some(canonical)));
        assert!(!editor_target_matches_active_work_dir(worktree, None));
    }
    use crate::state::{
        reported_session_shape_state, ChatMessageInfo, ChatStreamEvent, MessageRole,
        SubagentActivityStatus, ToolActivityInfo, TrajectoryDiagnostics, TrajectoryEntry,
    };

    #[test]
    fn markdown_cache_resets_only_after_its_limit() {
        assert!(!markdown_cache_exceeded(MARKDOWN_CACHE_ENTRY_LIMIT));
        assert!(markdown_cache_exceeded(MARKDOWN_CACHE_ENTRY_LIMIT + 1));
    }

    #[tokio::test]
    async fn chat_stream_batch_waits_then_caps_ready_events() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            next_chat_stream_batch(&mut rx),
        )
        .await
        .is_err());
        for index in 0..130 {
            tx.send(ChatStreamEvent::Finished {
                session_id: index.to_string(),
                session_file: std::path::PathBuf::new(),
            })
            .unwrap();
        }
        assert_eq!(next_chat_stream_batch(&mut rx).await.unwrap().len(), 128);
        assert_eq!(next_chat_stream_batch(&mut rx).await.unwrap().len(), 2);
    }

    #[test]
    fn trajectory_search_matches_ascii_without_case_sensitivity() {
        assert!(contains_case_insensitive("Read File", "read"));
        assert!(contains_case_insensitive("TOOL-CALL-42", "call-42"));
        assert!(!contains_case_insensitive("Write File", "read"));
    }

    #[test]
    fn subagent_popover_counts_items_without_owning_them() {
        assert_eq!(subagent_popover_counts([]), None);
        assert_eq!(
            subagent_popover_counts([
                SubagentActivityStatus::Queued,
                SubagentActivityStatus::Running,
                SubagentActivityStatus::Completed,
            ]),
            Some((3, 2))
        );
    }

    #[test]
    fn trajectory_search_preserves_unicode_lowercase_matching() {
        assert!(contains_case_insensitive("CAFÉ output", "café"));
        assert!(contains_case_insensitive("Kelvin", "kelvin"));
        assert!(!contains_case_insensitive("CAFÉ output", "résumé"));
    }

    #[test]
    fn chat_link_classifies_web_urls_as_external() {
        assert_eq!(
            classify_chat_link("https://example.com/spec"),
            ChatLinkTarget::Web
        );
        assert_eq!(
            classify_chat_link("http://example.com/spec"),
            ChatLinkTarget::Web
        );
    }

    #[test]
    fn chat_link_normalizes_safe_project_relative_paths() {
        assert_eq!(
            classify_chat_link("docs/spec.md"),
            ChatLinkTarget::ProjectFile("docs/spec.md".into())
        );
        assert_eq!(
            classify_chat_link("docs/design/../spec.md"),
            ChatLinkTarget::ProjectFile("docs/spec.md".into())
        );
    }

    #[test]
    fn chat_link_rejects_absolute_and_escaping_paths() {
        assert_eq!(classify_chat_link("/tmp/spec.md"), ChatLinkTarget::Rejected);
        assert_eq!(
            classify_chat_link("../../outside.md"),
            ChatLinkTarget::Rejected
        );
    }

    #[test]
    fn chat_link_does_not_parse_line_or_fragment_suffixes() {
        assert_eq!(
            classify_chat_link("src/main.rs:42"),
            ChatLinkTarget::ProjectFile("src/main.rs:42".into())
        );
        assert_eq!(
            classify_chat_link("src/main.rs#L42"),
            ChatLinkTarget::ProjectFile("src/main.rs#L42".into())
        );
    }
    fn metrics_with_usage(
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> ContextMeterMetrics {
        let billed_input_tokens = input_tokens
            .saturating_add(cache_read_tokens)
            .saturating_add(cache_write_tokens);
        ContextMeterMetrics {
            billed_input_tokens,
            output_tokens,
            cache_hit_percent: (billed_input_tokens > 0).then(|| {
                (((cache_read_tokens as u128) * 100 + (billed_input_tokens as u128) / 2)
                    / billed_input_tokens as u128) as u64
            }),
        }
    }

    fn estimating_context() -> ContextMeterContext {
        ContextMeterContext {
            current_tokens: 0,
            context_limit: 0,
            context_limit_is_estimate: false,
            effective_model: "new-model".into(),
            last_compaction_seq: None,
            provisional: false,
            estimating: true,
        }
    }

    #[test]
    fn a_model_that_never_reports_usage_is_not_shown_as_estimating() {
        // An ACP agent runs its own loop and sends no token accounting, so the
        // meter has nothing to project. Rendering that as "Estimating…" both
        // promises a number that never arrives and leaves the badge animating
        // for the whole turn.
        let view = context_meter_view_model(None, &ContextMeterMetrics::default(), false);
        assert_eq!(view.current_label, "Not reported");
        assert_eq!(view.percent, None);
        assert_eq!(view.bar_percent, 0.0);

        // A model that does report usage keeps the pending label until a
        // context figure arrives.
        let estimating = context_meter_view_model(None, &ContextMeterMetrics::default(), true);
        assert_eq!(estimating.current_label, "Estimating…");
    }

    #[test]
    fn an_unreported_context_still_shows_what_was_processed() {
        // Turn counts and any usage Threadlane did observe stay meaningful
        // even when the context window itself is unmeasurable.
        let view = context_meter_view_model(
            None,
            &ContextMeterMetrics {
                billed_input_tokens: 1_200,
                output_tokens: 800,
                cache_hit_percent: Some(40),
            },
            false,
        );
        assert_eq!(view.total_processed_label, "2.0k");
        assert_eq!(view.cache_hit_label.as_deref(), Some("40%"));
    }

    #[tokio::test]
    async fn meter_separates_current_context_from_total_processed() {
        let (path, state) = reported_session_shape_state().await;
        let projected_context = state.active_context_window().unwrap();
        let projected_metrics = state.active_session_metrics();
        let view = context_meter_view_model(
            Some(&ContextMeterContext {
                current_tokens: 38_278,
                context_limit: 128_000,
                context_limit_is_estimate: false,
                effective_model: "gpt-4o".into(),
                last_compaction_seq: None,
                provisional: false,
                estimating: false,
            }),
            &ContextMeterMetrics {
                billed_input_tokens: projected_metrics.billed_input_tokens(),
                output_tokens: projected_metrics.output_tokens,
                cache_hit_percent: projected_metrics.cache_hit_percent(),
            },
            true,
        );
        let percent = view.percent.expect("known context percentage");
        assert!((percent - 29.904_687_5).abs() < 1e-12);
        assert!((view.bar_percent - 29.904_687_5).abs() < 1e-12);
        assert_eq!(view.current_label, "38.3k / 128.0k");
        assert!(view.total_processed_label.ends_with('M'));
        assert_ne!(view.total_processed_label, view.current_label);
        assert!(view.cache_hit_label.is_some());
        assert_eq!(view.detail_label, "Context usage details, 30% used");
    }

    #[test]
    fn meter_estimating_context_has_no_false_percentage() {
        let view = context_meter_view_model(
            Some(&estimating_context()),
            &ContextMeterMetrics::default(),
            true,
        );
        assert_eq!(view.percent, None);
        assert_eq!(view.current_label, "Estimating…");
        assert_eq!(view.bar_percent, 0.0);
        assert_eq!(view.detail_label, "Context usage details, estimating usage");
    }

    #[test]
    fn meter_treats_zero_context_limit_as_unknown_even_when_not_estimating() {
        let mut context = estimating_context();
        context.current_tokens = 42;
        context.estimating = false;

        let view = context_meter_view_model(Some(&context), &ContextMeterMetrics::default(), true);

        assert_eq!(view.percent, None);
        assert_eq!(view.current_label, "Estimating…");
        assert_eq!(view.bar_percent, 0.0);
        assert_eq!(view.detail_label, "Context usage details, estimating usage");
    }

    #[test]
    fn meter_cache_hit_rounding_uses_wide_intermediates_at_u64_max() {
        let metrics = metrics_with_usage(0, 0, u64::MAX, 0);

        assert_eq!(metrics.billed_input_tokens, u64::MAX);
        assert_eq!(metrics.cache_hit_percent, Some(100));
    }

    #[test]
    fn meter_labels_estimated_limit_and_clamps_only_bar() {
        let view = context_meter_view_model(
            Some(&ContextMeterContext {
                current_tokens: 120_000,
                context_limit: 100_000,
                context_limit_is_estimate: true,
                effective_model: "model".into(),
                last_compaction_seq: Some(42),
                provisional: true,
                estimating: false,
            }),
            &ContextMeterMetrics::default(),
            true,
        );
        assert_eq!(view.percent, Some(120.0));
        assert_eq!(view.bar_percent, 100.0);
        assert_eq!(view.current_label, "120.0k / ~100.0k");
        assert_eq!(view.last_compaction_seq, Some(42));
        assert!(view.provisional);
    }

    #[test]
    fn markdown_update_appends_only_the_new_suffix() {
        assert_eq!(
            classify_markdown_update("Hello", "Hello **world**"),
            MarkdownUpdate::Append(" **world**")
        );
    }

    #[test]
    fn markdown_update_skips_identical_content() {
        assert_eq!(
            classify_markdown_update("Hello", "Hello"),
            MarkdownUpdate::Unchanged
        );
    }

    #[test]
    fn markdown_update_replaces_non_append_changes() {
        assert_eq!(
            classify_markdown_update("Hello", "Jello"),
            MarkdownUpdate::Replace
        );
        assert_eq!(
            classify_markdown_update("Hello", "Hello there"),
            MarkdownUpdate::Append(" there")
        );
        assert_eq!(
            classify_markdown_update("Hello there", "Hello"),
            MarkdownUpdate::Replace
        );
        assert_eq!(
            classify_markdown_update("Hello", "Hello!"),
            MarkdownUpdate::Append("!")
        );
        assert_eq!(
            classify_markdown_update("Hello", "Jello there"),
            MarkdownUpdate::Replace
        );
    }

    #[test]
    fn grouped_tool_activities_borrows_in_order_and_hides_plan_updates() {
        let activity_message = |activities: &[(&str, &str)]| ChatMessageInfo {
            id: activities[0].0.into(),
            role: MessageRole::Assistant,
            content: String::new(),
            tool_activities: activities
                .iter()
                .map(|(id, title)| ToolActivityInfo {
                    id: (*id).into(),
                    category: "tool".into(),
                    title: (*title).into(),
                    display_summary: String::new(),
                    detail: String::new(),
                    is_expanded: false,
                })
                .collect(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        };
        let messages = vec![
            activity_message(&[("tool-1", "read_file"), ("plan", "update_plan")]),
            activity_message(&[("tool-2", "write_file")]),
        ];

        let ids = grouped_tool_activities(&messages)
            .map(|activity| activity.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["tool-1", "tool-2"]);
    }

    #[test]
    fn transcript_rows_group_consecutive_tool_only_messages() {
        let message = |id: &str, activity: bool| ChatMessageInfo {
            id: id.into(),
            role: if activity {
                MessageRole::Assistant
            } else {
                MessageRole::User
            },
            content: if activity { "" } else { id }.into(),
            tool_activities: activity
                .then(|| ToolActivityInfo {
                    id: format!("tool-{id}"),
                    category: "tool".into(),
                    title: "read_file".into(),
                    display_summary: String::new(),
                    detail: String::new(),
                    is_expanded: false,
                })
                .into_iter()
                .collect(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        };
        let messages = vec![
            message("user", false),
            message("tool-1", true),
            message("tool-2", true),
            message("answer", false),
        ];

        assert_eq!(
            build_transcript_rows(&messages, true),
            vec![
                TranscriptRow::Message(0),
                TranscriptRow::Activities(1..3),
                TranscriptRow::Message(3),
                TranscriptRow::Working,
            ]
        );
    }

    #[test]
    fn selected_trajectory_entry_formats_as_raw_json() {
        let entry = TrajectoryEntry {
            seq: Some(1),
            run_id: None,
            turn: None,
            request: None,
            category: "Tool".into(),
            summary: "Read file".into(),
            detail: "src/main.rs".into(),
            lane: None,
            correlation_id: None,
            diagnostics: TrajectoryDiagnostics::default(),
        };

        let raw = format_trajectory_raw_json(&entry);

        assert!(raw.contains("\"category\": \"Tool\""));
        assert!(raw.contains("\"summary\": \"Read file\""));
    }

    fn trajectory_entry(
        category: &str,
        request: Option<u32>,
        turn: Option<u32>,
    ) -> TrajectoryEntry {
        TrajectoryEntry {
            seq: None,
            run_id: None,
            turn,
            request,
            category: category.into(),
            summary: category.into(),
            detail: String::new(),
            lane: None,
            correlation_id: None,
            diagnostics: TrajectoryDiagnostics::default(),
        }
    }

    #[test]
    fn trajectory_cache_reuses_entries_for_append_only_updates() {
        let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
        let source = vec![
            cached[0].clone(),
            trajectory_entry("Tool", Some(1), Some(1)),
        ];

        assert_eq!(reconcile_trajectory_entries(cached, &source), source);
    }

    #[test]
    fn trajectory_cache_replaces_entries_when_existing_data_changes() {
        let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
        let source = vec![trajectory_entry("Assistant", Some(1), Some(1))];

        assert_eq!(reconcile_trajectory_entries(cached, &source), source);
    }

    #[test]
    fn trajectory_epoch_distinguishes_append_from_replacement() {
        let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
        let appended_source = vec![
            cached[0].clone(),
            trajectory_entry("Tool", Some(1), Some(1)),
        ];
        let (entries, appended) =
            reconcile_trajectory_entries_by_epoch(cached.clone(), &appended_source, 7, 7);
        assert!(appended);
        assert_eq!(entries, appended_source);

        let replacement = vec![trajectory_entry("Assistant", Some(1), Some(1))];
        let (entries, appended) = reconcile_trajectory_entries_by_epoch(cached, &replacement, 7, 8);
        assert!(!appended);
        assert_eq!(entries, replacement);
    }

    #[test]
    fn trajectory_incremental_facets_match_full_rebuild() {
        let mut input = trajectory_entry("Input", Some(1), Some(1));
        input.lane = Some("main".into());
        let mut tool = trajectory_entry("Tool", Some(1), Some(1));
        tool.lane = Some("main".into());
        let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(2));
        anomaly.lane = Some("child".into());
        let appended_tool = trajectory_entry("Tool", Some(1), Some(2));
        let entries = vec![input, tool, anomaly, appended_tool];
        let key = TrajectoryCacheKey {
            revision: 4,
            epoch: 1,
            mode: TrajectoryMode::Execution,
            query: "tool".into(),
            category: None,
            lane: None,
        };

        let mut incremental = (Vec::new(), std::collections::BTreeMap::new(), Vec::new());
        extend_trajectory_facets(
            &mut incremental.0,
            &mut incremental.1,
            &mut incremental.2,
            &entries[..2],
            0,
            &key,
        );
        extend_trajectory_facets(
            &mut incremental.0,
            &mut incremental.1,
            &mut incremental.2,
            &entries,
            2,
            &key,
        );

        let mut rebuilt = (Vec::new(), std::collections::BTreeMap::new(), Vec::new());
        extend_trajectory_facets(
            &mut rebuilt.0,
            &mut rebuilt.1,
            &mut rebuilt.2,
            &entries,
            0,
            &key,
        );
        assert_eq!(incremental, rebuilt);
        assert_eq!(incremental.0, vec!["Anomaly", "Input", "Tool"]);
        assert_eq!(incremental.2, vec![1, 3]);
    }

    #[test]
    fn trajectory_previews_extend_without_reformatting_existing_entries() {
        let mut input = trajectory_entry("Input", Some(1), Some(1));
        input.summary = "Prompt".into();
        input.detail = "first\nsecond".into();
        let mut tool = trajectory_entry("Tool", Some(1), Some(1));
        tool.summary = "read_file".into();
        tool.detail.clear();
        let entries = vec![input, tool];
        let mut previews = Vec::new();

        extend_trajectory_previews(&mut previews, &entries[..1], 0);
        extend_trajectory_previews(&mut previews, &entries, 1);

        assert_eq!(previews, ["Prompt  first second", "read_file"]);
    }

    #[test]
    fn trajectory_rows_preserve_request_headers_and_setup_boundaries() {
        let entries = vec![
            trajectory_entry("Provider", Some(1), Some(1)),
            trajectory_entry("Input", Some(1), Some(1)),
            trajectory_entry("Input", Some(2), Some(2)),
        ];

        assert_eq!(
            build_trajectory_rows(&entries, &[0, 1, 2], TrajectoryMode::Requests),
            vec![
                TrajectoryRow::RequestHeader(1),
                TrajectoryRow::Setup,
                TrajectoryRow::Entry(0),
                TrajectoryRow::Entry(1),
                TrajectoryRow::RequestHeader(2),
                TrajectoryRow::Entry(2),
            ]
        );
    }

    #[test]
    fn trajectory_incremental_rows_match_full_rebuild() {
        let entries = vec![
            trajectory_entry("Provider", Some(1), Some(1)),
            trajectory_entry("Input", Some(1), Some(1)),
            trajectory_entry("Input", Some(2), Some(2)),
        ];
        let indices = [0, 1, 2];
        let mut incremental = Vec::new();
        extend_trajectory_rows(
            &mut incremental,
            &entries,
            &indices[..2],
            0,
            TrajectoryMode::Requests,
        );
        extend_trajectory_rows(
            &mut incremental,
            &entries,
            &indices,
            2,
            TrajectoryMode::Requests,
        );

        assert_eq!(
            incremental,
            build_trajectory_rows(&entries, &indices, TrajectoryMode::Requests)
        );
    }

    #[test]
    fn trajectory_summary_is_computed_once_from_canonical_entries() {
        let mut tool = trajectory_entry("Tool", Some(1), Some(3));
        tool.diagnostics.duration_ms = Some(25);
        let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(4));
        anomaly.diagnostics.duration_ms = Some(75);
        anomaly.diagnostics.is_anomaly = true;

        let summary = summarize_trajectory(&[tool, anomaly]);

        assert_eq!(summary.tool_count, 1);
        assert_eq!(summary.total_duration_ms, 100);
        assert_eq!(summary.anomaly_count, 1);
        assert_eq!(summary.max_turn, 4);
    }

    #[test]
    fn trajectory_summary_append_matches_full_rebuild() {
        let initial = vec![trajectory_entry("Input", Some(1), Some(1))];
        let mut tool = trajectory_entry("Tool", Some(1), Some(1));
        tool.diagnostics.duration_ms = Some(25);
        let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(2));
        anomaly.diagnostics.duration_ms = Some(75);
        let appended = vec![tool, anomaly];
        let mut incremental = summarize_trajectory(&initial);
        extend_trajectory_summary(&mut incremental, &appended);

        let rebuilt =
            summarize_trajectory(&initial.into_iter().chain(appended).collect::<Vec<_>>());
        assert_eq!(incremental.overview_positions, rebuilt.overview_positions);
        assert_eq!(incremental.tool_count, rebuilt.tool_count);
        assert_eq!(incremental.total_duration_ms, 100);
        assert_eq!(incremental.anomaly_count, rebuilt.anomaly_count);
        assert_eq!(incremental.max_turn, rebuilt.max_turn);
    }
    #[test]
    fn trajectory_cache_key_changes_with_data_or_filter() {
        let base = TrajectoryCacheKey {
            revision: 7,
            epoch: 2,
            mode: TrajectoryMode::Execution,
            query: "tool".into(),
            category: None,
            lane: None,
        };
        let mut changed = base.clone();
        changed.revision += 1;
        assert_ne!(base, changed);

        let mut changed = base.clone();
        changed.query = "provider".into();
        assert_ne!(base, changed);
    }

    #[test]
    fn extract_markdown_segments_parses_mixed_content_with_paths() {
        let input = "Here is the command:\n```bash\ncargo build --workspace\n```\nAnd code in file:\n```rust src/main.rs\n// src/main.rs\nfn main() {}\n```\nFinished!";
        let segments = extract_markdown_segments(input);
        assert_eq!(segments.len(), 5);
        assert_eq!(
            segments[0],
            MarkdownSegment::Markdown("Here is the command:\n".into())
        );
        assert_eq!(
            segments[1],
            MarkdownSegment::CodeBlock {
                language: "bash".into(),
                header_path: None,
                code: "cargo build --workspace\n".into(),
            }
        );
        assert_eq!(
            segments[2],
            MarkdownSegment::Markdown("And code in file:\n".into())
        );
        assert_eq!(
            segments[3],
            MarkdownSegment::CodeBlock {
                language: "rust".into(),
                header_path: Some("src/main.rs".into()),
                code: "// src/main.rs\nfn main() {}\n".into(),
            }
        );
        assert_eq!(segments[4], MarkdownSegment::Markdown("Finished!".into()));
    }

    #[test]
    fn extract_markdown_segments_handles_comment_path_heuristic() {
        let input =
            "```typescript\n// app/routes/index.tsx\nexport default function Home() {}\n```";
        let segments = extract_markdown_segments(input);
        assert_eq!(segments.len(), 1);
        assert_eq!(
            segments[0],
            MarkdownSegment::CodeBlock {
                language: "typescript".into(),
                header_path: Some("app/routes/index.tsx".into()),
                code: "// app/routes/index.tsx\nexport default function Home() {}\n".into(),
            }
        );
    }

    #[test]
    fn extract_markdown_segments_rejects_version_and_shebang_as_paths() {
        for input in [
            "```sh\n#!/bin/sh\necho hi\n```",
            "```rust\n// v1.2\nfn main() {}\n```",
        ] {
            let segments = extract_markdown_segments(input);
            assert!(matches!(
                segments.as_slice(),
                [MarkdownSegment::CodeBlock {
                    header_path: None,
                    ..
                }]
            ));
        }
    }

    #[test]
    fn extract_markdown_segments_accepts_indented_closing_fence() {
        let segments = extract_markdown_segments("```rust\nlet x = 1;\n  ```\nAfter");
        assert!(
            matches!(segments.as_slice(), [MarkdownSegment::CodeBlock { .. }, MarkdownSegment::Markdown(text)] if text == "After")
        );
    }

    #[test]
    fn extract_markdown_segments_keeps_text_before_mid_line_backticks() {
        let segments = extract_markdown_segments("Prefix ```inline\n```rust\ncode\n```");
        assert!(
            matches!(segments.as_slice(), [MarkdownSegment::Markdown(text), MarkdownSegment::CodeBlock { language, .. }] if text == "Prefix ```inline\n" && language == "rust")
        );
    }

    #[test]
    fn normalize_terminal_command_removes_prompt_markers() {
        assert_eq!(
            normalize_terminal_command("$ echo one\n>>> echo two\n> echo three"),
            "echo one\necho two\necho three"
        );
    }

    #[test]
    fn terminal_runnable_language_identifies_shell_flavors() {
        assert!(is_terminal_runnable_language("bash"));
        assert!(is_terminal_runnable_language("sh"));
        assert!(is_terminal_runnable_language("zsh"));
        assert!(is_terminal_runnable_language("shell"));
        assert!(!is_terminal_runnable_language("rust"));
        assert!(!is_terminal_runnable_language("python"));
        assert!(!is_terminal_runnable_language("json"));
    }

    #[test]
    fn active_slash_command_query_identifies_autocomplete_prefixes() {
        assert_eq!(active_slash_command_query("/"), Some(""));
        assert_eq!(active_slash_command_query("/com"), Some("com"));
        assert_eq!(active_slash_command_query("  /help"), Some("help"));
        assert_eq!(active_slash_command_query("/commit message"), None);
        assert_eq!(active_slash_command_query("/commit "), None);
        assert_eq!(active_slash_command_query("hello /help"), None);
        assert_eq!(active_slash_command_query("plain text"), None);
    }

    #[test]
    fn slash_command_binding_context_matches_nested_input() {
        use gpui::{KeyBindingContextPredicate, KeyContext};

        let predicate = KeyBindingContextPredicate::parse(SLASH_COMMAND_BINDING_CONTEXT).unwrap();
        let menu_ctx = KeyContext::try_from(SLASH_COMMAND_KEY_CONTEXT).unwrap();
        let input_ctx = KeyContext::try_from(INPUT_KEY_CONTEXT).unwrap();

        let active_contexts = vec![menu_ctx, input_ctx.clone()];
        assert_eq!(predicate.depth_of(&active_contexts), Some(2));
        assert!(predicate.eval(&active_contexts));

        let normal_contexts = vec![input_ctx];
        assert_eq!(predicate.depth_of(&normal_contexts), None);
        assert!(!predicate.eval(&normal_contexts));
    }
}
