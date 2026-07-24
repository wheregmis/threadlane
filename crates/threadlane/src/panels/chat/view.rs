//! Chat panel main view & transcript list widget.

use super::state::{ChatMessage, MsgRole, StreamingKind, ToolIcon, ToolStatus};
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

fn update_activity_status(cx: &mut Cx, item_widget: &WidgetRef, running: bool, error: bool) {
    let indicator = item_widget.widget(cx, ids!(status_indicator));
    indicator
        .widget(cx, ids!(status_running_indicator))
        .set_visible(cx, running);
    indicator
        .widget(cx, ids!(status_done_indicator))
        .set_visible(cx, !running && !error);
    indicator
        .widget(cx, ids!(status_error_lbl))
        .set_visible(cx, !running && error);
}

#[derive(Clone, Copy)]
enum DisplayRow {
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

fn is_activity(message: &ChatMessage) -> bool {
    matches!(
        message,
        ChatMessage::Thinking { .. } | ChatMessage::Tool { .. }
    )
}

fn display_rows(
    messages: &[ChatMessage],
    streaming_kind: Option<StreamingKind>,
    streaming_text: &str,
) -> Vec<DisplayRow> {
    let mut rows = Vec::new();

    for (message_index, message) in messages.iter().enumerate() {
        if is_activity(message) {
            if let Some(DisplayRow::ActivityGroup { end, .. }) = rows.last_mut() {
                if *end == message_index {
                    *end = message_index + 1;
                    continue;
                }
            }
            rows.push(DisplayRow::ActivityGroup {
                start: message_index,
                end: message_index + 1,
                streaming_thinking: false,
            });
        } else {
            rows.push(DisplayRow::Message(message_index));
        }
    }

    if !streaming_text.is_empty() {
        match streaming_kind {
            Some(StreamingKind::Thinking) => {
                if let Some(DisplayRow::ActivityGroup {
                    end,
                    streaming_thinking,
                    ..
                }) = rows.last_mut()
                {
                    if *end == messages.len() {
                        *streaming_thinking = true;
                    } else {
                        rows.push(DisplayRow::ActivityGroup {
                            start: messages.len(),
                            end: messages.len(),
                            streaming_thinking: true,
                        });
                    }
                } else {
                    rows.push(DisplayRow::ActivityGroup {
                        start: messages.len(),
                        end: messages.len(),
                        streaming_thinking: true,
                    });
                }
            }
            _ => rows.push(DisplayRow::StreamingAssistant),
        }
    }

    rows
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
    if parts.is_empty() && has_thinking {
        parts.push("Reasoned".to_string());
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
        ToolStatus::Done if !result_metadata.is_empty() => {
            line.push_str(&format!(" · {}", markdown_inline(result_metadata)))
        }
        ToolStatus::Done => {}
    }
    line
}

#[derive(Script, ScriptHook, Widget)]
pub struct ChatList {
    #[deref]
    view: View,
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
        let rows = display_rows(&data.messages, data.streaming_kind, &data.streaming_text);

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, rows.len());

                while let Some(item_id) = list.next_visible_item(cx) {
                    let Some(row) = rows.get(item_id).copied() else {
                        continue;
                    };
                    match row {
                        DisplayRow::StreamingAssistant => {
                            let item_widget = list.item(cx, item_id, id!(AssistantMsg));
                            item_widget
                                .markdown(cx, ids!(md))
                                .set_text(cx, &data.streaming_text);
                            item_widget.draw_all_unscoped(cx);
                        }
                        DisplayRow::ActivityGroup {
                            start,
                            end,
                            streaming_thinking,
                        } => {
                            let item_widget = list.item(cx, item_id, id!(ActivityGroupMsg));
                            let mut counts = ActivityCounts::default();
                            let mut lines = Vec::new();
                            let mut has_thinking = streaming_thinking;
                            let mut running = streaming_thinking;
                            let mut has_error = false;
                            let mut first_icon = None;
                            let mut mixed_icons = false;

                            for message in &data.messages[start..end] {
                                match message {
                                    ChatMessage::Thinking { .. } => has_thinking = true,
                                    ChatMessage::Tool {
                                        name,
                                        status,
                                        presentation,
                                        result_metadata,
                                        ..
                                    } => {
                                        let kind = activity_kind(name, presentation.icon);
                                        match kind {
                                            ActivityKind::ExploredFile => {
                                                counts.explored_files += 1
                                            }
                                            ActivityKind::ExploredFolder => {
                                                counts.explored_folders += 1
                                            }
                                            ActivityKind::Search => counts.searches += 1,
                                            ActivityKind::Edited => counts.edited += 1,
                                            ActivityKind::Command => counts.commands += 1,
                                            ActivityKind::Skill => counts.skills += 1,
                                            ActivityKind::Delegated => counts.delegated += 1,
                                            ActivityKind::Other => counts.other += 1,
                                        }
                                        running |= *status == ToolStatus::Running;
                                        has_error |= *status == ToolStatus::Error;
                                        if let Some(icon) = first_icon {
                                            mixed_icons |= icon != presentation.icon;
                                        } else {
                                            first_icon = Some(presentation.icon);
                                        }
                                        lines.push(activity_line(
                                            kind,
                                            &presentation.title,
                                            &presentation.primary,
                                            result_metadata,
                                            *status,
                                        ));
                                    }
                                    ChatMessage::Text { .. } => {}
                                }
                            }

                            if streaming_thinking {
                                lines.push("- **Thinking…**".to_string());
                            } else if lines.is_empty() && has_thinking {
                                lines.push("- Reasoning completed.".to_string());
                            }

                            show_tool_icon(
                                cx,
                                &item_widget,
                                if mixed_icons {
                                    ToolIcon::Generic
                                } else {
                                    first_icon.unwrap_or(ToolIcon::Generic)
                                },
                            );
                            item_widget
                                .label(cx, ids!(title_lbl))
                                .set_text(cx, if running { "Working" } else { "Worked" });
                            item_widget
                                .label(cx, ids!(preview_lbl))
                                .set_text(cx, &activity_preview(&counts, has_thinking));
                            update_activity_status(cx, &item_widget, running, has_error);
                            item_widget
                                .markdown(cx, ids!(md))
                                .set_text(cx, &lines.join("\n"));
                            item_widget.draw_all_unscoped(cx);
                        }
                        DisplayRow::Message(message_index) => {
                            let Some(message) = data.messages.get(message_index) else {
                                continue;
                            };
                            match message {
                                ChatMessage::Text { role, text } => match role {
                                    MsgRole::User => {
                                        let item_widget = list.item(cx, item_id, id!(UserMsg));
                                        item_widget.markdown(cx, ids!(md)).set_text(cx, text);
                                        item_widget.draw_all_unscoped(cx);
                                    }
                                    MsgRole::Assistant => {
                                        let item_widget = list.item(cx, item_id, id!(AssistantMsg));
                                        item_widget.markdown(cx, ids!(md)).set_text(cx, text);
                                        item_widget.draw_all_unscoped(cx);
                                    }
                                    MsgRole::System => {
                                        let item_widget = list.item(cx, item_id, id!(SystemMsg));
                                        item_widget.label(cx, ids!(lbl)).set_text(cx, text);
                                        item_widget.draw_all_unscoped(cx);
                                    }
                                },
                                ChatMessage::Thinking { text } => {
                                    let item_widget = list.item(cx, item_id, id!(ThinkingMsg));
                                    item_widget.markdown(cx, ids!(md)).set_text(cx, text);
                                    item_widget.label(cx, ids!(preview_lbl)).set_text(
                                        cx,
                                        &super::state::collapsed_thinking_preview(text, 72),
                                    );
                                    item_widget.draw_all_unscoped(cx);
                                }
                                ChatMessage::Tool {
                                    output,
                                    status,
                                    presentation,
                                    result_preview,
                                    result_metadata,
                                    ..
                                } => {
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
                                    );
                                    item_widget
                                        .widget(cx, ids!(args_section))
                                        .label(cx, ids!(content_lbl))
                                        .set_text(cx, &presentation.arguments_detail);
                                    let arguments_are_fully_summarized = matches!(
                                        presentation.icon,
                                        ToolIcon::ReadFile
                                            | ToolIcon::ListDirectory
                                            | ToolIcon::Skill
                                    );
                                    item_widget.widget(cx, ids!(args_section)).set_visible(
                                        cx,
                                        !arguments_are_fully_summarized
                                            && !presentation.arguments_detail.is_empty(),
                                    );
                                    let output_detail =
                                        super::state::tool_result_detail(output, 6_000);
                                    let result_section =
                                        item_widget.widget(cx, ids!(result_section));
                                    result_section
                                        .label(cx, ids!(content_lbl))
                                        .set_text(cx, &output_detail);
                                    result_section
                                        .widget(cx, ids!(content_lbl))
                                        .set_visible(cx, !presentation.output_markdown);
                                    let content_md_wrap =
                                        result_section.widget(cx, ids!(content_md_wrap));
                                    content_md_wrap
                                        .markdown(cx, ids!(content_md))
                                        .set_text(cx, &output_detail);
                                    content_md_wrap.set_visible(cx, presentation.output_markdown);
                                    result_section.set_visible(cx, !output.is_empty());
                                    item_widget.draw_all_unscoped(cx);
                                }
                            }
                        }
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
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
        assert!(matches!(
            rows[0],
            DisplayRow::ActivityGroup {
                start: 0,
                end: 3,
                streaming_thinking: false
            }
        ));
        assert!(matches!(rows[1], DisplayRow::Message(3)));
    }

    #[test]
    fn streaming_thinking_merges_into_trailing_activity_group() {
        let messages = vec![tool("read", "read_file", r#"{"path":"src/app.rs"}"#)];

        let rows = display_rows(&messages, Some(StreamingKind::Thinking), "Reviewing");
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            DisplayRow::ActivityGroup {
                start: 0,
                end: 1,
                streaming_thinking: true
            }
        ));
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
    }
}
