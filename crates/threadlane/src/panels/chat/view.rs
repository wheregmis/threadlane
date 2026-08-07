//! Chat panel main view & transcript list widget.

use super::state::{
    ChatMessage, HarnessActivity, HarnessActivityStatus, MsgRole, StreamingKind, SubagentRailItem,
    ToolIcon, ToolStatus,
};
use crate::components::tool_fold_header::ToolFoldHeaderAction;
use crate::path_utils::{compact_workspace_path, truncate_chars};
use crate::workspace::AppState;
use makepad_widgets::*;

const TOOL_ICON_MAP: [(ToolIcon, &[LiveId; 1]); 8] = [
    (ToolIcon::Generic, ids!(icon_generic)),
    (ToolIcon::ReadFile, ids!(icon_read_file)),
    (ToolIcon::WriteFile, ids!(icon_write_file)),
    (ToolIcon::EditFile, ids!(icon_edit_file)),
    (ToolIcon::ListDirectory, ids!(icon_list_directory)),
    (ToolIcon::Terminal, ids!(icon_terminal)),
    (ToolIcon::Skill, ids!(icon_skill)),
    (ToolIcon::Subagent, ids!(icon_subagent)),
];

fn show_tool_icon(cx: &mut Cx, item: &WidgetRef, selected: ToolIcon) {
    for (icon, id) in TOOL_ICON_MAP {
        item.widget(cx, id).set_visible(cx, selected == icon);
    }
}

fn update_activity_status(
    cx: &mut Cx,
    item_widget: &WidgetRef,
    running: bool,
    error: bool,
    cancelled: bool,
) {
    let indicator = item_widget.widget(cx, ids!(status_indicator));
    indicator
        .widget(cx, ids!(status_running_indicator))
        .set_visible(cx, running);
    indicator
        .widget(cx, ids!(status_done_indicator))
        .set_visible(cx, !running && !error && !cancelled);
    indicator
        .widget(cx, ids!(status_cancelled_indicator))
        .set_visible(cx, !running && !error && cancelled);
    indicator
        .widget(cx, ids!(status_error_lbl))
        .set_visible(cx, !running && error);
}

#[derive(Clone, Debug)]
struct CachedActivityGroup {
    detail: String,
    preview: String,
    title: &'static str,
    tool_icon: ToolIcon,
    running: bool,
    has_error: bool,
    has_cancelled: bool,
}

#[derive(Clone, Debug)]
struct CachedSubagentTool {
    rail_items: Vec<SubagentRailItem>,
    preview: String,
}

#[derive(Clone, Debug)]
struct CachedTool {
    message_index: usize,
    output_detail: String,
}

#[derive(Clone, Debug)]
enum DisplayRow {
    Message(usize),
    SubagentTool(CachedSubagentTool),
    Tool(CachedTool),
    ActivityGroup(CachedActivityGroup),
    StreamingAssistant,
}

#[derive(Clone, Copy)]
enum InterimRow {
    Message(usize),
    ActivityGroup {
        start: usize,
        end: usize,
        streaming_thinking: bool,
    },
    StreamingAssistant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityKind {
    ExploredFile,
    ExploredFolder,
    Search,
    Edited,
    Command,
    Skill,
    Delegated,
    Other,
}

#[derive(Default)]
struct ActivityCounts {
    explored_files: usize,
    explored_folders: usize,
    searches: usize,
    edited: usize,
    commands: usize,
    skills: usize,
    delegated: usize,
    other: usize,
}

impl ActivityCounts {
    fn add(&mut self, kind: ActivityKind) {
        match kind {
            ActivityKind::ExploredFile => self.explored_files += 1,
            ActivityKind::ExploredFolder => self.explored_folders += 1,
            ActivityKind::Search => self.searches += 1,
            ActivityKind::Edited => self.edited += 1,
            ActivityKind::Command => self.commands += 1,
            ActivityKind::Skill => self.skills += 1,
            ActivityKind::Delegated => self.delegated += 1,
            ActivityKind::Other => self.other += 1,
        }
    }
}

fn is_activity(message: &ChatMessage) -> bool {
    match message {
        ChatMessage::Thinking { .. } => true,
        ChatMessage::Tool {
            name, presentation, ..
        } => name != "subagent" && presentation.icon != ToolIcon::Subagent,
        _ => false,
    }
}

#[cfg(test)]
fn display_rows(
    messages: &[ChatMessage],
    streaming_kind: Option<StreamingKind>,
    streaming_text: &str,
) -> Vec<DisplayRow> {
    display_rows_with_harness(messages, streaming_kind, streaming_text, &[])
}

fn display_rows_with_harness(
    messages: &[ChatMessage],
    streaming_kind: Option<StreamingKind>,
    streaming_text: &str,
    activities: &[HarnessActivity],
) -> Vec<DisplayRow> {
    let mut interim = Vec::new();
    let owned_subagent_runs = super::state::owned_subagent_child_runs(messages);
    let owned_runs = owned_subagent_runs.values().copied().collect::<Vec<_>>();

    for (message_index, message) in messages.iter().enumerate() {
        if super::state::is_owned_subagent_child_tool(message, &owned_runs) {
            continue;
        }
        if is_activity(message) {
            if let Some(InterimRow::ActivityGroup { end, .. }) = interim.last_mut() {
                if *end == message_index {
                    *end = message_index + 1;
                    continue;
                }
            }
            interim.push(InterimRow::ActivityGroup {
                start: message_index,
                end: message_index + 1,
                streaming_thinking: false,
            });
        } else {
            interim.push(InterimRow::Message(message_index));
        }
    }

    if !streaming_text.is_empty() {
        match streaming_kind {
            Some(StreamingKind::Thinking) => {
                if let Some(InterimRow::ActivityGroup {
                    end,
                    streaming_thinking,
                    ..
                }) = interim.last_mut()
                {
                    if *end == messages.len() {
                        *streaming_thinking = true;
                    } else {
                        interim.push(InterimRow::ActivityGroup {
                            start: messages.len(),
                            end: messages.len(),
                            streaming_thinking: true,
                        });
                    }
                } else {
                    interim.push(InterimRow::ActivityGroup {
                        start: messages.len(),
                        end: messages.len(),
                        streaming_thinking: true,
                    });
                }
            }
            _ => interim.push(InterimRow::StreamingAssistant),
        }
    }

    let mut rows = interim
        .into_iter()
        .map(|row| match row {
            InterimRow::StreamingAssistant => DisplayRow::StreamingAssistant,
            InterimRow::ActivityGroup {
                start,
                end,
                streaming_thinking,
            } => {
                let mut counts = ActivityCounts::default();
                let mut has_thinking = streaming_thinking;
                let mut running = streaming_thinking;
                let mut has_error = false;
                let mut has_cancelled = false;
                let mut first_icon = None;
                let mut mixed_icons = false;

                if start < messages.len() {
                    let group_end = end.min(messages.len());
                    for message in &messages[start..group_end] {
                        match message {
                            ChatMessage::Thinking { .. } => has_thinking = true,
                            ChatMessage::Tool {
                                name,
                                status,
                                presentation,
                                ..
                            } => {
                                let kind = activity_kind(name, presentation.icon);
                                counts.add(kind);
                                running |= *status == ToolStatus::Running;
                                has_error |= *status == ToolStatus::Error;
                                has_cancelled |= *status == ToolStatus::Cancelled;
                                if let Some(icon) = first_icon {
                                    mixed_icons |= icon != presentation.icon;
                                } else {
                                    first_icon = Some(presentation.icon);
                                }
                            }
                            ChatMessage::Text { .. } => {}
                        }
                    }
                }

                let detail = activity_detail(
                    if start < messages.len() {
                        &messages[start..end.min(messages.len())]
                    } else {
                        &[]
                    },
                    streaming_thinking.then_some(streaming_text),
                );

                let title = if running {
                    "Working"
                } else if has_cancelled {
                    "Stopped"
                } else {
                    "Worked"
                };

                let preview = activity_preview(&counts, has_thinking);

                let tool_icon = if mixed_icons {
                    ToolIcon::Generic
                } else {
                    first_icon.unwrap_or(ToolIcon::Generic)
                };

                DisplayRow::ActivityGroup(CachedActivityGroup {
                    detail,
                    preview,
                    title,
                    tool_icon,
                    running,
                    has_error,
                    has_cancelled,
                })
            }
            InterimRow::Message(message_index) => {
                let Some(message) = messages.get(message_index) else {
                    return DisplayRow::Message(message_index);
                };
                match message {
                    ChatMessage::Tool {
                        name,
                        arguments,
                        output,
                        status,
                        presentation,
                        result_metadata,
                        ..
                    } => {
                        if name == "subagent" || presentation.icon == ToolIcon::Subagent {
                            let rail_items = super::state::subagent_rail_items_with_harness(
                                arguments,
                                output,
                                *status,
                                messages,
                                owned_subagent_runs.get(&message_index).copied(),
                            );
                            let working = rail_items
                                .iter()
                                .filter(|item| item.status == "Working")
                                .count();
                            let queued = rail_items
                                .iter()
                                .filter(|item| item.status == "Queued")
                                .count();
                            let preview = if *status == ToolStatus::Running {
                                let suffix = if queued > 0 {
                                    format!(" · {queued} queued")
                                } else {
                                    String::new()
                                };
                                format!(
                                    "{} agent{} · {working} working{suffix}",
                                    rail_items.len(),
                                    if rail_items.len() == 1 { "" } else { "s" },
                                )
                            } else {
                                format!(
                                    "{} agent{} · {}",
                                    rail_items.len(),
                                    if rail_items.len() == 1 { "" } else { "s" },
                                    result_metadata,
                                )
                            };
                            DisplayRow::SubagentTool(CachedSubagentTool {
                                rail_items,
                                preview,
                            })
                        } else {
                            let output_detail = super::state::tool_result_detail(output, 6_000);
                            DisplayRow::Tool(CachedTool {
                                message_index,
                                output_detail,
                            })
                        }
                    }
                    _ => DisplayRow::Message(message_index),
                }
            }
        })
        .collect::<Vec<_>>();

    let mut matched = vec![false; activities.len()];
    for row in &mut rows {
        let DisplayRow::SubagentTool(row) = row else {
            continue;
        };
        let mut row_activities = Vec::new();
        for (index, activity) in activities.iter().enumerate() {
            if row
                .rail_items
                .iter()
                .any(|item| item.key.as_deref() == Some(activity.key.as_str()))
            {
                matched[index] = true;
                row_activities.push(activity.clone());
            }
        }
        for item in row.rail_items.iter_mut().filter(|item| item.key.is_none()) {
            let Some((index, activity)) =
                activities.iter().enumerate().find(|(index, activity)| {
                    !matched[*index]
                        && item.task
                            == super::state::normalize_whitespace_bounded(&activity.task, 160)
                        && item.agent == activity.agent
                })
            else {
                continue;
            };
            item.key = Some(activity.key.clone());
            matched[index] = true;
            row_activities.push(activity.clone());
        }
        if !row_activities.is_empty() {
            super::state::merge_harness_activities(&mut row.rail_items, &row_activities);
            row.preview = harness_activity_preview(&row_activities);
        }
    }

    let unmatched = activities
        .iter()
        .enumerate()
        .filter_map(|(index, activity)| (!matched[index]).then_some(activity))
        .collect::<Vec<_>>();
    if !unmatched.is_empty() {
        rows.push(DisplayRow::ActivityGroup(harness_activity_group(
            &unmatched,
        )));
    }

    rows
}

fn harness_activity_preview(activities: &[HarnessActivity]) -> String {
    let status = [
        HarnessActivityStatus::Aborted,
        HarnessActivityStatus::Faulted,
        HarnessActivityStatus::Retrying,
        HarnessActivityStatus::Recovering,
        HarnessActivityStatus::Working,
        HarnessActivityStatus::Queued,
        HarnessActivityStatus::Recovered,
        HarnessActivityStatus::Cancelled,
    ]
    .into_iter()
    .find(|status| activities.iter().any(|activity| activity.status == *status))
    .expect("non-empty harness activities");
    let label = super::state::harness_activity_label(
        activities
            .iter()
            .find(|activity| activity.status == status)
            .expect("selected harness activity status"),
    );
    let count = activities.len();
    format!(
        "{label} · {count} {}",
        if count == 1 { "task" } else { "tasks" }
    )
}

fn harness_activity_group(activities: &[&HarnessActivity]) -> CachedActivityGroup {
    let running = activities.iter().any(|activity| {
        matches!(
            activity.status,
            HarnessActivityStatus::Queued
                | HarnessActivityStatus::Working
                | HarnessActivityStatus::Recovering
                | HarnessActivityStatus::Retrying
        )
    });
    let has_error = activities.iter().any(|activity| {
        matches!(
            activity.status,
            HarnessActivityStatus::Aborted
                | HarnessActivityStatus::Faulted
                | HarnessActivityStatus::Retrying
        )
    });
    let has_cancelled = activities
        .iter()
        .any(|activity| activity.status == HarnessActivityStatus::Cancelled);
    let detail = activities
        .iter()
        .map(|activity| {
            format!(
                "- {} — {}",
                super::state::normalize_whitespace_bounded(&activity.task, 240),
                super::state::harness_activity_detail(activity)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    CachedActivityGroup {
        detail,
        preview: harness_activity_preview(
            &activities
                .iter()
                .map(|activity| (*activity).clone())
                .collect::<Vec<_>>(),
        ),
        title: if running { "Working" } else { "Worked" },
        tool_icon: ToolIcon::Generic,
        running,
        has_error,
        has_cancelled,
    }
}

fn activity_kind(name: &str, icon: ToolIcon) -> ActivityKind {
    let normalized = name.to_ascii_lowercase();
    if icon == ToolIcon::ListDirectory || normalized.contains("list") {
        ActivityKind::ExploredFolder
    } else if normalized.contains("search")
        || normalized.contains("grep")
        || normalized.contains("find")
    {
        ActivityKind::Search
    } else if icon == ToolIcon::ReadFile || normalized.contains("read") {
        ActivityKind::ExploredFile
    } else if matches!(icon, ToolIcon::WriteFile | ToolIcon::EditFile)
        || normalized.contains("write")
        || normalized.contains("edit")
    {
        ActivityKind::Edited
    } else if icon == ToolIcon::Terminal
        || normalized.contains("command")
        || normalized.contains("terminal")
        || normalized.contains("shell")
    {
        ActivityKind::Command
    } else if icon == ToolIcon::Skill || normalized.contains("skill") {
        ActivityKind::Skill
    } else if icon == ToolIcon::Subagent
        || normalized.contains("subagent")
        || normalized.contains("delegate")
    {
        ActivityKind::Delegated
    } else {
        ActivityKind::Other
    }
}

fn pluralized(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn activity_preview(counts: &ActivityCounts, has_thinking: bool) -> String {
    let mut parts = Vec::new();
    if has_thinking {
        parts.push("Reasoned".to_string());
    }
    let mut explored = Vec::new();
    if counts.explored_files > 0 {
        explored.push(pluralized(counts.explored_files, "file", "files"));
    }
    if counts.explored_folders > 0 {
        explored.push(pluralized(counts.explored_folders, "folder", "folders"));
    }
    if counts.searches > 0 {
        explored.push(pluralized(counts.searches, "search", "searches"));
    }
    if !explored.is_empty() {
        parts.push(format!("Explored {}", explored.join(", ")));
    }
    if counts.edited > 0 {
        parts.push(format!(
            "Edited {}",
            pluralized(counts.edited, "file", "files")
        ));
    }
    if counts.commands > 0 {
        parts.push(format!(
            "Ran {}",
            pluralized(counts.commands, "command", "commands")
        ));
    }
    if counts.skills > 0 {
        parts.push(format!(
            "Loaded {}",
            pluralized(counts.skills, "skill", "skills")
        ));
    }
    if counts.delegated > 0 {
        parts.push(format!(
            "Delegated {}",
            pluralized(counts.delegated, "task", "tasks")
        ));
    }
    if counts.other > 0 {
        parts.push(format!(
            "Used {}",
            pluralized(counts.other, "tool", "tools")
        ));
    }
    parts.join(" · ")
}

fn markdown_inline(text: &str) -> String {
    text.replace(['\r', '\n'], " ").replace('`', "'")
}

fn activity_line(
    kind: ActivityKind,
    title: &str,
    primary: &str,
    result_metadata: &str,
    status: ToolStatus,
) -> String {
    let action = match kind {
        ActivityKind::ExploredFile | ActivityKind::ExploredFolder | ActivityKind::Search => {
            "Explored"
        }
        ActivityKind::Edited => "Edited",
        ActivityKind::Command => "Ran command",
        ActivityKind::Skill => "Loaded skill",
        ActivityKind::Delegated => "Delegated",
        ActivityKind::Other => title,
    };
    let mut line = format!("- **{}**", markdown_inline(action));
    if !primary.is_empty() {
        line.push_str(&format!(" `{}`", markdown_inline(primary)));
    }
    match status {
        ToolStatus::Running => line.push_str(" · Running"),
        ToolStatus::Error => line.push_str(" · Failed"),
        ToolStatus::Cancelled if !result_metadata.is_empty() => {
            line.push_str(&format!(" · {}", markdown_inline(result_metadata)))
        }
        ToolStatus::Cancelled => line.push_str(" · Stopped"),
        ToolStatus::Done if !result_metadata.is_empty() => {
            line.push_str(&format!(" · {}", markdown_inline(result_metadata)))
        }
        ToolStatus::Done => {}
    }
    line
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityDetailKind {
    Thinking,
    Tool,
}

fn append_activity_detail(
    detail: &mut String,
    previous_kind: &mut Option<ActivityDetailKind>,
    kind: ActivityDetailKind,
    block: &str,
) {
    if block.is_empty() {
        return;
    }
    if !detail.is_empty() {
        if *previous_kind == Some(ActivityDetailKind::Tool) && kind == ActivityDetailKind::Tool {
            detail.push('\n');
        } else {
            detail.push_str("\n\n");
        }
    }
    detail.push_str(block);
    *previous_kind = Some(kind);
}

fn activity_detail(messages: &[ChatMessage], streaming_thinking: Option<&str>) -> String {
    let mut detail = String::new();
    let mut previous_kind = None;
    let mut has_thinking = false;

    for message in messages {
        match message {
            ChatMessage::Thinking { text } => {
                has_thinking = true;
                if !text.trim().is_empty() {
                    append_activity_detail(
                        &mut detail,
                        &mut previous_kind,
                        ActivityDetailKind::Thinking,
                        &format!("**Thinking**\n\n{text}"),
                    );
                }
            }
            ChatMessage::Tool {
                name,
                status,
                presentation,
                result_metadata,
                output,
                ..
            } => {
                let kind = activity_kind(name, presentation.icon);
                let mut line = activity_line(
                    kind,
                    &presentation.title,
                    &presentation.primary,
                    result_metadata,
                    *status,
                );
                if name == "subagent" || presentation.icon == ToolIcon::Subagent {
                    if !presentation.arguments_detail.is_empty() {
                        line.push_str("\n\n");
                        line.push_str(&presentation.arguments_detail);
                    }
                    if !output.trim().is_empty() {
                        line.push_str("\n\n");
                        line.push_str(output.trim());
                    }
                }
                append_activity_detail(
                    &mut detail,
                    &mut previous_kind,
                    ActivityDetailKind::Tool,
                    &line,
                );
            }
            ChatMessage::Text { .. } => {}
        }
    }

    if let Some(text) = streaming_thinking {
        has_thinking = true;
        let block = if text.trim().is_empty() {
            "**Thinking…**".to_string()
        } else {
            format!("**Thinking…**\n\n{text}")
        };
        append_activity_detail(
            &mut detail,
            &mut previous_kind,
            ActivityDetailKind::Thinking,
            &block,
        );
    }

    if detail.is_empty() && has_thinking {
        "Reasoning completed.".to_string()
    } else {
        detail
    }
}

fn user_message_needs_wrapping(text: &str) -> bool {
    const COMPACT_LINE_CHAR_LIMIT: usize = 88;

    text.lines()
        .any(|line| line.chars().count() > COMPACT_LINE_CHAR_LIMIT)
}

fn draw_markdown_item(
    list: &mut PortalList,
    cx: &mut Cx2d,
    item_id: usize,
    template: LiveId,
    text: &str,
) {
    let item_widget = list.item(cx, item_id, template);
    let mut md = item_widget.markdown(cx, ids!(md));
    if md.text() != text {
        md.set_text(cx, text);
    }
    item_widget.draw_all_unscoped(cx);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StarterPromptAction {
    Explore,
    Build,
    Review,
    Fix,
    #[default]
    None,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ChatList {
    #[deref]
    view: View,
    /// Cached display rows; rebuilt only when message count or streaming kind changes.
    #[rust]
    cached_rows: Vec<DisplayRow>,
    #[rust]
    cached_base_rows: Vec<DisplayRow>,
    #[rust]
    cached_base_revision: u64,
    #[rust]
    cached_msg_count: usize,
    #[rust]
    cached_streaming_kind: Option<StreamingKind>,
    #[rust]
    cached_streaming_text_len: usize,
    #[rust]
    cached_revision: u64,
    #[rust]
    hovered_starter: Option<StarterPromptAction>,
    #[rust]
    pressed_starter: Option<StarterPromptAction>,
    #[rust]
    hovered_jump_to_latest: bool,
}

#[derive(Script, ScriptHook, Widget)]
pub struct SubagentRail {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[live]
    row_template: ScriptValue,
    #[rust]
    rows: ComponentMap<LiveId, WidgetRef>,
    #[rust]
    pub items: Vec<SubagentRailItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubagentRailAction {
    Resume(String),
    Abort(String),
}

impl Widget for SubagentRail {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for row in self.rows.values_mut() {
            row.handle_event(cx, event, scope);
        }
        if let Event::Actions(actions) = event {
            for (index, item) in self.items.iter().enumerate() {
                if item.status != "Recovering" {
                    continue;
                }
                let row_id = LiveId::from_num(1, index as u64);
                let Some(row) = self.rows.get_mut(&row_id) else {
                    continue;
                };
                let Some(key) = item.key.as_ref() else {
                    continue;
                };
                if row.button(cx, ids!(resume_btn)).clicked(actions) {
                    cx.widget_action(self.uid, SubagentRailAction::Resume(key.clone()));
                } else if row.button(cx, ids!(abort_btn)).clicked(actions) {
                    cx.widget_action(self.uid, SubagentRailAction::Abort(key.clone()));
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.draw_bg.begin(cx, walk, self.layout);
        let item_count = self.items.len();
        self.rows.retain(|row_id, _| {
            (0..item_count).any(|index| *row_id == LiveId::from_num(1, index as u64))
        });
        for (index, item) in self.items.iter().enumerate() {
            let row_id = LiveId::from_num(1, index as u64);
            let template = self.row_template;
            let row = self.rows.get_or_insert(cx, row_id, |cx| {
                cx.with_vm(|vm| WidgetRef::script_from_value(vm, template))
            });
            let display_agent = if item.parent_lane.is_some() {
                format!("└─ {}", item.agent)
            } else {
                item.agent.clone()
            };
            let title = row.label(cx, ids!(title_lbl));
            if title.text() != display_agent {
                title.set_text(cx, &display_agent);
            }
            let preview = row.label(cx, ids!(preview_lbl));
            if preview.text() != item.task {
                preview.set_text(cx, &item.task);
            }
            let display_status = if let Some(usage) = &item.token_usage {
                format!("{} ({})", item.status, usage)
            } else {
                item.status.clone()
            };
            let status = row.label(cx, ids!(status_lbl));
            if status.text() != display_status {
                status.set_text(cx, &display_status);
            }
            let mut md = row.markdown(cx, ids!(detail_md));
            if md.text() != item.detail {
                md.set_text(cx, &item.detail);
            }
            row.widget(cx, ids!(working_detail)).set_visible(
                cx,
                item.status == "Working" && item.detail.trim().is_empty(),
            );
            row.button(cx, ids!(resume_btn))
                .set_visible(cx, item.status == "Recovering");
            row.button(cx, ids!(abort_btn))
                .set_visible(cx, item.status == "Recovering");
            update_activity_status(
                cx,
                row,
                item.status == "Working",
                item.status == "Failed",
                item.status == "Stopped",
            );
            row.draw_all_unscoped(cx);
        }
        self.draw_bg.end(cx);
        DrawStep::done()
    }
}

impl Widget for ChatList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(data) = scope
            .data
            .get::<AppState>()
            .and_then(AppState::active_workspace)
            .map(|workspace| workspace.chat.clone())
        else {
            return DrawStep::done();
        };

        // Rebuild display rows only when the message list or streaming state changes.
        let msg_count = data.messages.len();
        let streaming_text_len = data.streaming_text.len();
        if msg_count != self.cached_msg_count
            || data.streaming_kind != self.cached_streaming_kind
            || streaming_text_len != self.cached_streaming_text_len
            || data.revision != self.cached_revision
        {
            if msg_count != self.cached_msg_count || data.revision != self.cached_base_revision {
                self.cached_base_rows =
                    display_rows_with_harness(&data.messages, None, "", &data.harness_activities);
                self.cached_base_revision = data.revision;
            }

            if data.streaming_kind == Some(StreamingKind::Assistant) {
                self.cached_rows = self.cached_base_rows.clone();
                if !data.streaming_text.is_empty() {
                    self.cached_rows.push(DisplayRow::StreamingAssistant);
                }
            } else {
                self.cached_rows = display_rows_with_harness(
                    &data.messages,
                    data.streaming_kind,
                    &data.streaming_text,
                    &data.harness_activities,
                );
            }
            self.cached_msg_count = msg_count;
            self.cached_streaming_kind = data.streaming_kind;
            self.cached_streaming_text_len = streaming_text_len;
            self.cached_revision = data.revision;
        }
        let rows = &self.cached_rows;

        let is_empty = data.messages.is_empty()
            && data.streaming_text.is_empty()
            && data.harness_activities.is_empty();

        // Toggle the empty-state overlay — it lives as a sibling to the PortalList
        // so it can use height: Fill and truly center its content.
        let empty_state = self.view.widget(cx, ids!(empty_state));
        if is_empty {
            if let Some(key) = scope.data.get::<AppState>().and_then(|s| s.active_key()) {
                let name = key
                    .work_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| key.work_dir.display().to_string());
                let name = truncate_chars(&name, 40);
                empty_state
                    .label(cx, ids!(project_name_inline_lbl))
                    .set_text(cx, &name);
                let home_dir = std::env::var_os("HOME").map(std::path::PathBuf::from);
                let path = compact_workspace_path(&key.work_dir, home_dir.as_deref());
                empty_state
                    .label(cx, ids!(workspace_path_lbl))
                    .set_text(cx, &path);
            }
        }
        empty_state.set_visible(cx, is_empty);
        // The PortalList is the later sibling in this overlay and otherwise sits above the
        // welcome cards, intercepting their pointer events even when it has no rows.
        self.view.widget(cx, ids!(list)).set_visible(cx, !is_empty);

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                // Markdown remeasurement changes item heights after the list has chosen its
                // viewport. Preserve the user's bottom-lock explicitly so PortalList does not
                // animate the full stale overflow back into view.
                let was_at_end = list.is_at_end();
                list.set_tail_range(was_at_end);
                list.set_item_range(cx, 0, rows.len());

                while let Some(item_id) = list.next_visible_item(cx) {
                    let Some(row) = rows.get(item_id) else {
                        continue;
                    };

                    match row {
                        DisplayRow::StreamingAssistant => {
                            draw_markdown_item(
                                &mut list,
                                cx,
                                item_id,
                                id!(AssistantMsg),
                                &data.streaming_text,
                            );
                        }
                        DisplayRow::ActivityGroup(group) => {
                            let item_widget = list.item(cx, item_id, id!(ActivityGroupMsg));
                            show_tool_icon(cx, &item_widget, group.tool_icon);
                            item_widget
                                .label(cx, ids!(title_lbl))
                                .set_text(cx, group.title);
                            item_widget
                                .label(cx, ids!(preview_lbl))
                                .set_text(cx, &group.preview);
                            update_activity_status(
                                cx,
                                &item_widget,
                                group.running,
                                group.has_error,
                                group.has_cancelled,
                            );
                            let mut md = item_widget.markdown(cx, ids!(md));
                            if md.text() != group.detail {
                                md.set_text(cx, &group.detail);
                            }
                            item_widget.draw_all_unscoped(cx);
                        }
                        DisplayRow::SubagentTool(tool) => {
                            let item_widget = list.item(cx, item_id, id!(SubagentMsg));
                            item_widget
                                .label(cx, ids!(preview_lbl))
                                .set_text(cx, &tool.preview);
                            update_activity_status(
                                cx,
                                &item_widget,
                                tool.rail_items.iter().any(|item| {
                                    matches!(item.status.as_str(), "Working" | "Recovering")
                                }),
                                tool.rail_items.iter().any(|item| {
                                    matches!(
                                        item.status.as_str(),
                                        "Retrying recovery" | "Aborted · unsafe tool"
                                    )
                                }),
                                tool.rail_items
                                    .iter()
                                    .any(|item| item.status == "Cancelled"),
                            );
                            let rail = item_widget.widget(cx, ids!(rail));
                            if let Some(mut rail) = rail.as_subagent_rail().borrow_mut() {
                                if rail.items != tool.rail_items {
                                    rail.items = tool.rail_items.clone();
                                }
                            }
                            item_widget.draw_all_unscoped(cx);
                        }
                        DisplayRow::Tool(tool) => {
                            let Some(message) = data.messages.get(tool.message_index) else {
                                continue;
                            };
                            let ChatMessage::Tool {
                                status,
                                presentation,
                                result_preview,
                                result_metadata,
                                output,
                                ..
                            } = message
                            else {
                                continue;
                            };

                            let item_widget = list.item(cx, item_id, id!(ToolMsg));
                            show_tool_icon(cx, &item_widget, presentation.icon);
                            item_widget
                                .label(cx, ids!(title_lbl))
                                .set_text(cx, &presentation.title);
                            item_widget
                                .label(cx, ids!(meta_lbl))
                                .set_text(cx, &presentation.metadata);
                            item_widget
                                .widget(cx, ids!(meta_lbl))
                                .set_visible(cx, !presentation.metadata.is_empty());
                            item_widget
                                .label(cx, ids!(preview_lbl))
                                .set_text(cx, &presentation.primary);
                            item_widget
                                .label(cx, ids!(result_meta_lbl))
                                .set_text(cx, result_metadata);
                            item_widget
                                .widget(cx, ids!(result_meta_lbl))
                                .set_visible(cx, !result_metadata.is_empty());

                            let has_completed_result = *status != ToolStatus::Running;
                            item_widget
                                .label(cx, ids!(result_preview_lbl))
                                .set_text(cx, result_preview);
                            item_widget
                                .widget(cx, ids!(result_preview_lbl))
                                .set_visible(
                                    cx,
                                    has_completed_result && !result_preview.is_empty(),
                                );
                            item_widget
                                .label(cx, ids!(result_meta_header_lbl))
                                .set_text(cx, result_metadata);
                            item_widget
                                .widget(cx, ids!(result_meta_header_lbl))
                                .set_visible(
                                    cx,
                                    has_completed_result && !result_metadata.is_empty(),
                                );

                            update_activity_status(
                                cx,
                                &item_widget,
                                *status == ToolStatus::Running,
                                *status == ToolStatus::Error,
                                *status == ToolStatus::Cancelled,
                            );

                            let args_section = item_widget.widget(cx, ids!(args_section));
                            let content_lbl = args_section.label(cx, ids!(content_lbl));
                            if content_lbl.text() != presentation.arguments_detail {
                                content_lbl.set_text(cx, &presentation.arguments_detail);
                            }
                            let arguments_are_fully_summarized = matches!(
                                presentation.icon,
                                ToolIcon::ReadFile | ToolIcon::ListDirectory | ToolIcon::Skill
                            );
                            args_section.set_visible(
                                cx,
                                !arguments_are_fully_summarized
                                    && !presentation.arguments_detail.is_empty(),
                            );

                            let result_section = item_widget.widget(cx, ids!(result_section));
                            let res_lbl = result_section.label(cx, ids!(content_lbl));
                            if res_lbl.text() != tool.output_detail {
                                res_lbl.set_text(cx, &tool.output_detail);
                            }
                            result_section
                                .widget(cx, ids!(content_lbl))
                                .set_visible(cx, !presentation.output_markdown);

                            let content_md_wrap = result_section.widget(cx, ids!(content_md_wrap));
                            let mut md = content_md_wrap.markdown(cx, ids!(content_md));
                            if md.text() != tool.output_detail {
                                md.set_text(cx, &tool.output_detail);
                            }
                            content_md_wrap.set_visible(cx, presentation.output_markdown);
                            result_section.set_visible(cx, !output.is_empty());
                            item_widget.draw_all_unscoped(cx);
                        }
                        DisplayRow::Message(message_index) => {
                            let Some(message) = data.messages.get(*message_index) else {
                                continue;
                            };
                            match message {
                                ChatMessage::Text { role, text } => match role {
                                    MsgRole::User => {
                                        let template = if user_message_needs_wrapping(text) {
                                            id!(UserMsgWrapped)
                                        } else {
                                            id!(UserMsg)
                                        };
                                        draw_markdown_item(&mut list, cx, item_id, template, text);
                                    }
                                    MsgRole::Assistant => {
                                        draw_markdown_item(
                                            &mut list,
                                            cx,
                                            item_id,
                                            id!(AssistantMsg),
                                            text,
                                        );
                                    }
                                    MsgRole::System => {
                                        let item_widget = list.item(cx, item_id, id!(SystemMsg));
                                        let lbl = item_widget.label(cx, ids!(lbl));
                                        if lbl.text() != *text {
                                            lbl.set_text(cx, text);
                                        }
                                        item_widget.draw_all_unscoped(cx);
                                    }
                                },
                                ChatMessage::Thinking { text } => {
                                    let item_widget = list.item(cx, item_id, id!(ThinkingMsg));
                                    let mut md = item_widget.markdown(cx, ids!(md));
                                    if md.text() != *text {
                                        md.set_text(cx, text);
                                    }
                                    let preview_lbl = item_widget.label(cx, ids!(preview_lbl));
                                    let preview_text =
                                        super::state::collapsed_thinking_preview(text, 72);
                                    if preview_lbl.text() != preview_text {
                                        preview_lbl.set_text(cx, &preview_text);
                                    }
                                    item_widget.draw_all_unscoped(cx);
                                }
                                ChatMessage::Tool { .. } => {}
                            }
                        }
                    }
                }

                let can_jump_to_latest = !list.is_at_end() && !rows.is_empty();
                let jump_layer = self.view.widget(cx, ids!(jump_to_latest_layer));
                jump_layer.set_visible(cx, can_jump_to_latest);
                jump_layer.redraw(cx);
                let jump_button = self.view.button(cx, ids!(jump_to_latest_btn));
                jump_button.set_visible(cx, can_jump_to_latest);
                jump_button.redraw(cx);
                let jump_hint = self.view.widget(cx, ids!(jump_to_latest_hint));
                jump_hint.set_visible(cx, can_jump_to_latest && self.hovered_jump_to_latest);
                jump_hint.redraw(cx);
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event {
            Event::MouseMove(mouse_event) => {
                let hovered = self.starter_prompt_at(cx, mouse_event.abs);
                self.set_starter_feedback(cx, hovered, self.pressed_starter);
            }
            Event::MouseDown(mouse_event) if mouse_event.button.is_primary() => {
                let pressed = self.starter_prompt_at(cx, mouse_event.abs);
                self.set_starter_feedback(cx, pressed, pressed);
            }
            Event::MouseUp(mouse_event) if mouse_event.button.is_primary() => {
                let hovered = self.starter_prompt_at(cx, mouse_event.abs);
                self.set_starter_feedback(cx, hovered, None);
            }
            Event::MouseLeave(_) => self.set_starter_feedback(cx, None, None),
            _ => {}
        }
        self.view.handle_event(cx, event, scope);

        if let Event::Actions(actions) = event {
            let list = self.view.portal_list(cx, ids!(list));
            if list.scrolled(actions) {
                let can_jump_to_latest = !list.is_at_end() && !self.cached_rows.is_empty();
                let jump_layer = self.view.widget(cx, ids!(jump_to_latest_layer));
                jump_layer.set_visible(cx, can_jump_to_latest);
                jump_layer.redraw(cx);
                let jump_button = self.view.button(cx, ids!(jump_to_latest_btn));
                jump_button.set_visible(cx, can_jump_to_latest);
                jump_button.redraw(cx);
                if !can_jump_to_latest {
                    self.hovered_jump_to_latest = false;
                    let jump_hint = self.view.widget(cx, ids!(jump_to_latest_hint));
                    jump_hint.set_visible(cx, false);
                    jump_hint.redraw(cx);
                }
            }
            if list.smooth_scroll_reached(actions) {
                list.set_tail_range(true);
            }

            let jump_button_ref = self.view.widget(cx, ids!(jump_to_latest_btn));
            let jump_button = self.view.button(cx, ids!(jump_to_latest_btn));
            let jump_button_view = jump_button_ref.as_view();
            if jump_button_view.finger_hover_in(actions).is_some() {
                self.hovered_jump_to_latest = true;
                self.view.widget(cx, ids!(jump_to_latest_hint)).redraw(cx);
            } else if jump_button_view.finger_hover_out(actions).is_some() {
                self.hovered_jump_to_latest = false;
                let jump_hint = self.view.widget(cx, ids!(jump_to_latest_hint));
                jump_hint.set_visible(cx, false);
                jump_hint.redraw(cx);
            }
            if jump_button.clicked(actions) {
                list.set_tail_range(false);
                list.smooth_scroll_to_end(cx, 12.0, None);
                let jump_layer = self.view.widget(cx, ids!(jump_to_latest_layer));
                jump_layer.set_visible(cx, false);
                jump_layer.redraw(cx);
                jump_button.set_visible(cx, false);
                jump_button.redraw(cx);
                self.hovered_jump_to_latest = false;
                let jump_hint = self.view.widget(cx, ids!(jump_to_latest_hint));
                jump_hint.set_visible(cx, false);
                jump_hint.redraw(cx);
                self.view.redraw(cx);
            }

            let list = self.view.portal_list(cx, ids!(list));
            let layout_changed = actions.iter().any(|action| {
                action.downcast_ref::<WidgetAction>().is_some_and(|action| {
                    matches!(
                        action.cast::<ToolFoldHeaderAction>(),
                        ToolFoldHeaderAction::LayoutChanged
                    )
                })
            });
            if layout_changed {
                list.redraw(cx);
            }
        }

        if matches!(event, Event::KeyDown(key_event) if matches!(key_event.key_code, KeyCode::ReturnKey | KeyCode::Space))
        {
            if let Some(action) = self.focused_starter_action(cx) {
                cx.action(action);
            }
        }
    }
}

impl ChatList {
    fn focused_starter_action(&self, cx: &Cx) -> Option<StarterPromptAction> {
        [
            (
                ids!(empty_state.cards_row.explore_card.btn),
                StarterPromptAction::Explore,
            ),
            (
                ids!(empty_state.cards_row.build_card.btn),
                StarterPromptAction::Build,
            ),
            (
                ids!(empty_state.cards_row.review_card.btn),
                StarterPromptAction::Review,
            ),
            (
                ids!(empty_state.cards_row.fix_card.btn),
                StarterPromptAction::Fix,
            ),
        ]
        .into_iter()
        .find_map(|(path, action)| {
            cx.has_key_focus(self.view.widget(cx, path).area())
                .then_some(action)
        })
    }

    fn set_starter_feedback(
        &mut self,
        cx: &mut Cx,
        hovered: Option<StarterPromptAction>,
        pressed: Option<StarterPromptAction>,
    ) {
        if self.hovered_starter == hovered && self.pressed_starter == pressed {
            return;
        }
        self.hovered_starter = hovered;
        self.pressed_starter = pressed;

        for (path, action) in [
            (
                ids!(empty_state.cards_row.explore_card),
                StarterPromptAction::Explore,
            ),
            (
                ids!(empty_state.cards_row.build_card),
                StarterPromptAction::Build,
            ),
            (
                ids!(empty_state.cards_row.review_card),
                StarterPromptAction::Review,
            ),
            (
                ids!(empty_state.cards_row.fix_card),
                StarterPromptAction::Fix,
            ),
        ] {
            let (color, border_color) = if pressed == Some(action) {
                (
                    vec4(0.145, 0.188, 0.247, 1.0),
                    vec4(0.337, 0.463, 0.624, 1.0),
                )
            } else if hovered == Some(action) {
                (
                    vec4(0.129, 0.165, 0.212, 1.0),
                    vec4(0.247, 0.322, 0.412, 1.0),
                )
            } else {
                (
                    vec4(0.114, 0.137, 0.173, 1.0),
                    vec4(0.165, 0.204, 0.255, 1.0),
                )
            };
            let mut card = self.view.widget(cx, path);
            script_apply_eval!(cx, card, {
                draw_bg +: {
                    color: #(color)
                    border_color: #(border_color)
                }
            });
            card.redraw(cx);
        }
    }

    fn starter_prompt_at(&self, cx: &Cx, position: Vec2d) -> Option<StarterPromptAction> {
        if !self.view.widget(cx, ids!(empty_state)).visible() {
            return None;
        }
        let cards = [
            (
                ids!(empty_state.cards_row.explore_card),
                StarterPromptAction::Explore,
            ),
            (
                ids!(empty_state.cards_row.build_card),
                StarterPromptAction::Build,
            ),
            (
                ids!(empty_state.cards_row.review_card),
                StarterPromptAction::Review,
            ),
            (
                ids!(empty_state.cards_row.fix_card),
                StarterPromptAction::Fix,
            ),
        ];
        cards.into_iter().find_map(|(path, action)| {
            self.view
                .widget(cx, path)
                .area()
                .rect(cx)
                .contains(position)
                .then_some(action)
        })
    }
}

impl ChatListRef {
    pub fn starter_prompt_at(&self, cx: &Cx, position: Vec2d) -> Option<StarterPromptAction> {
        self.borrow()
            .and_then(|inner| inner.starter_prompt_at(cx, position))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn tool(id: &str, name: &str, arguments: &str) -> ChatMessage {
        ChatMessage::Tool {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
            output: String::new(),
            status: ToolStatus::Done,
            presentation: super::super::state::tool_presentation(name, arguments),
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        }
    }

    #[test]
    fn long_user_lines_use_the_wrapped_message_layout() {
        assert!(!user_message_needs_wrapping("A short user message"));
        assert!(!user_message_needs_wrapping(
            "Several short lines\nstill stay compact"
        ));
        assert!(user_message_needs_wrapping(&"word ".repeat(90)));
    }

    #[test]
    fn consecutive_activity_messages_share_one_display_row() {
        let messages = vec![
            ChatMessage::Thinking {
                text: "Plan".into(),
            },
            tool("read", "read_file", r#"{"path":"src/app.rs"}"#),
            tool("edit", "edit_file", r#"{"path":"src/app.rs","edits":[]}"#),
            ChatMessage::Text {
                role: MsgRole::Assistant,
                text: "Done".into(),
            },
        ];

        let rows = display_rows(&messages, None, "");
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0], DisplayRow::ActivityGroup(_)));
        assert!(matches!(rows[1], DisplayRow::Message(3)));
    }

    #[test]
    fn standalone_harness_activity_uses_the_worked_activity_group() {
        let activities = vec![super::super::state::HarnessActivity {
            key: "lane-a".into(),
            task: "Recover interrupted work".into(),
            agent: "scout".into(),
            status: super::super::state::HarnessActivityStatus::Recovering,
            detail: "Recovered checkpoint".into(),
        }];

        let rows = display_rows_with_harness(&[], None, "", &activities);

        assert_eq!(rows.len(), 1);
        let DisplayRow::ActivityGroup(row) = &rows[0] else {
            panic!("expected a worked activity group");
        };
        assert_eq!(row.title, "Working");
        assert_eq!(row.preview, "Recovering · 1 task");
        assert!(row.detail.contains("Recover interrupted work"));
    }

    #[test]
    fn harness_activity_updates_repeated_task_names_by_durable_key() {
        let subagent = |id: &str, run_id: &str| ChatMessage::Tool {
            id: id.into(),
            name: "subagent".into(),
            arguments: "{}".into(),
            output: serde_json::json!([{
                "run_id": run_id,
                "task": "Same task",
                "agent": "scout",
                "status": "Done",
                "thinking": "",
                "inner_tools": [],
                "output": "completed"
            }])
            .to_string(),
            status: ToolStatus::Done,
            presentation: super::super::state::tool_presentation("subagent", "{}"),
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        };
        let messages = vec![
            subagent("delegate-a", "lane-a"),
            subagent("delegate-b", "lane-b"),
        ];
        let activities = vec![
            super::super::state::HarnessActivity {
                key: "lane-a".into(),
                task: "Same task".into(),
                agent: "scout".into(),
                status: super::super::state::HarnessActivityStatus::Recovered,
                detail: "First complete".into(),
            },
            super::super::state::HarnessActivity {
                key: "lane-b".into(),
                task: "Same task".into(),
                agent: "scout".into(),
                status: super::super::state::HarnessActivityStatus::Recovering,
                detail: "Second recovering".into(),
            },
        ];

        let rows = display_rows_with_harness(&messages, None, "", &activities);

        assert_eq!(rows.len(), 2);
        let [DisplayRow::SubagentTool(first), DisplayRow::SubagentTool(second)] = &rows[..] else {
            panic!("expected two delegation rows");
        };
        assert_eq!(first.rail_items[0].key.as_deref(), Some("lane-a"));
        assert_eq!(first.rail_items[0].status, "Recovered");
        assert_eq!(second.rail_items[0].key.as_deref(), Some("lane-b"));
        assert_eq!(second.rail_items[0].status, "Recovering");
        assert_eq!(second.rail_items.len(), 1);
    }

    #[test]
    fn harness_activity_preserves_keys_for_multiple_sessions_in_one_result() {
        let message = ChatMessage::Tool {
            id: "delegate".into(),
            name: "subagent".into(),
            arguments: "{}".into(),
            output: serde_json::json!([
                {
                    "run_id": "lane-a",
                    "task": "First task",
                    "agent": "scout",
                    "status": "Done",
                    "thinking": "",
                    "inner_tools": [],
                    "output": "completed"
                },
                {
                    "run_id": "lane-b",
                    "task": "Second task",
                    "agent": "reviewer",
                    "status": "Done",
                    "thinking": "",
                    "inner_tools": [],
                    "output": "completed"
                }
            ])
            .to_string(),
            status: ToolStatus::Done,
            presentation: super::super::state::tool_presentation("subagent", "{}"),
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        };
        let activities = vec![
            super::super::state::HarnessActivity {
                key: "lane-a".into(),
                task: "First task".into(),
                agent: "scout".into(),
                status: super::super::state::HarnessActivityStatus::Recovered,
                detail: "First complete".into(),
            },
            super::super::state::HarnessActivity {
                key: "lane-b".into(),
                task: "Second task".into(),
                agent: "reviewer".into(),
                status: super::super::state::HarnessActivityStatus::Recovering,
                detail: "Second recovering".into(),
            },
        ];

        let rows = display_rows_with_harness(&[message], None, "", &activities);

        assert_eq!(rows.len(), 1);
        let DisplayRow::SubagentTool(row) = &rows[0] else {
            panic!("expected one delegation row");
        };
        assert_eq!(row.rail_items.len(), 2);
        assert_eq!(row.rail_items[0].key.as_deref(), Some("lane-a"));
        assert_eq!(row.rail_items[0].status, "Recovered");
        assert_eq!(row.rail_items[1].key.as_deref(), Some("lane-b"));
        assert_eq!(row.rail_items[1].status, "Recovering");
        assert_eq!(row.preview, "Recovering · 2 tasks");
    }

    #[test]
    fn cancelled_live_subagent_activity_updates_the_existing_tool_row() {
        let arguments = serde_json::json!({
            "parallel": true,
            "tasks": [{"agent": "scout", "task": "Inspect the repository"}]
        })
        .to_string();
        let message = ChatMessage::Tool {
            id: "delegate".into(),
            name: "subagent".into(),
            arguments,
            output: String::new(),
            status: ToolStatus::Running,
            presentation: super::super::state::tool_presentation("subagent", "{}"),
            result_preview: String::new(),
            result_metadata: String::new(),
            started_at: Instant::now(),
        };
        let activities = vec![super::super::state::HarnessActivity {
            key: "subagent-run-1".into(),
            task: "Inspect the repository".into(),
            agent: "scout".into(),
            status: super::super::state::HarnessActivityStatus::Cancelled,
            detail: "Cancelled".into(),
        }];

        let rows = display_rows_with_harness(&[message], None, "", &activities);

        assert_eq!(rows.len(), 1);
        let DisplayRow::SubagentTool(row) = &rows[0] else {
            panic!("expected one delegation row");
        };
        assert_eq!(row.rail_items[0].status, "Cancelled");
        assert_eq!(row.preview, "Cancelled · 1 task");
    }

    #[test]
    fn child_tool_rows_are_hidden_only_when_a_subagent_parent_is_rendered() {
        let child = tool(
            "subagent-404:0:read",
            "read_file",
            r#"{"path":"src/app.rs"}"#,
        );

        let orphaned_rows = display_rows(std::slice::from_ref(&child), None, "");
        assert_eq!(orphaned_rows.len(), 1);
        assert!(matches!(orphaned_rows[0], DisplayRow::ActivityGroup(_)));

        let unrelated_child = tool(
            "subagent-405:0:read",
            "read_file",
            r#"{"path":"src/unrelated.rs"}"#,
        );
        let owned_rows = display_rows(
            &[tool("delegate", "subagent", "{}"), child, unrelated_child],
            None,
            "",
        );
        assert_eq!(owned_rows.len(), 2);
        assert!(matches!(owned_rows[0], DisplayRow::SubagentTool(_)));
        assert!(matches!(owned_rows[1], DisplayRow::ActivityGroup(_)));
    }

    #[test]
    fn streaming_thinking_merges_into_trailing_activity_group() {
        let messages = vec![tool("read", "read_file", r#"{"path":"src/app.rs"}"#)];

        let rows = display_rows(&messages, Some(StreamingKind::Thinking), "Reviewing");
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], DisplayRow::ActivityGroup(_)));
    }

    #[test]
    fn activity_detail_preserves_finalized_and_streaming_thinking_in_order() {
        let completed = format!(
            "Starting analysis. {}Final persisted reasoning sentence.",
            "Detailed reasoning step. ".repeat(400)
        );
        let messages = vec![
            ChatMessage::Thinking {
                text: completed.clone(),
            },
            tool("read", "read_file", r#"{"path":"src/app.rs"}"#),
            ChatMessage::Thinking {
                text: "Reasoning after the tool.".into(),
            },
        ];

        let detail = activity_detail(&messages, Some("Current streaming reasoning."));

        assert!(detail.contains(&completed));
        let completed_index = detail.find("Final persisted reasoning sentence.").unwrap();
        let tool_index = detail.find("src/app.rs").unwrap();
        let resumed_index = detail.find("Reasoning after the tool.").unwrap();
        let streaming_index = detail.find("Current streaming reasoning.").unwrap();
        assert!(completed_index < tool_index);
        assert!(tool_index < resumed_index);
        assert!(resumed_index < streaming_index);
    }

    #[test]
    fn activity_preview_distinguishes_exploration_types() {
        let counts = ActivityCounts {
            explored_files: 2,
            explored_folders: 1,
            searches: 1,
            edited: 3,
            commands: 1,
            ..Default::default()
        };

        assert_eq!(
            activity_preview(&counts, false),
            "Explored 2 files, 1 folder, 1 search · Edited 3 files · Ran 1 command"
        );
        assert_eq!(
            activity_preview(&counts, true),
            "Reasoned · Explored 2 files, 1 folder, 1 search · Edited 3 files · Ran 1 command"
        );
    }
}
