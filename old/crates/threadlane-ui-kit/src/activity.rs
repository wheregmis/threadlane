//! Controlled tool result disclosure used by desktop and iOS.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};

/// Completed rows share one reveal; running, thinking, and failed rows remain
/// visible. Hosts own expansion and construct row content only when requested.
pub fn completed_activity_group<'a>(
    id: &str,
    expanded: bool,
    activities: impl Iterator<Item = &'a threadlane_protocol::daemon::ToolActivityInfo> + Clone,
    motion: &crate::DisclosureMotion,
    mut render_activity: impl FnMut(&threadlane_protocol::daemon::ToolActivityInfo) -> AnyElement,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    theme: &gpui_component::theme::ThemeColor,
) -> AnyElement {
    let needs_attention = |activity: &threadlane_protocol::daemon::ToolActivityInfo| {
        matches!(activity.category.as_str(), "Working" | "Thinking" | "Error")
    };
    let hidden_count = activities
        .clone()
        .filter(|activity| !needs_attention(activity))
        .count();
    let summary = tool_group_summary(
        activities
            .clone()
            .filter(|activity| !needs_attention(activity)),
    );
    let mut rows = Vec::new();
    let mut completed = Vec::new();
    let mut start_id = None;
    // Keep each contiguous completed run in its original position. Separate
    // parent IDs give each reveal its own measurement while sharing progress.
    let flush = |rows: &mut Vec<AnyElement>,
                 completed: &mut Vec<AnyElement>,
                 start_id: &mut Option<String>| {
        if let Some(start) = start_id.take() {
            rows.push(
                div()
                    .id(SharedString::from(format!("completed-run-{id}-{start}")))
                    .w_full()
                    .min_w_0()
                    .child(
                        motion.content(div().flex().flex_col().children(std::mem::take(completed))),
                    )
                    .into_any_element(),
            );
        }
    };
    for activity in activities {
        if needs_attention(activity) {
            flush(&mut rows, &mut completed, &mut start_id);
            rows.push(render_activity(activity));
        } else if motion.is_visible() {
            start_id.get_or_insert_with(|| activity.id.clone());
            completed.push(render_activity(activity));
        }
    }
    flush(&mut rows, &mut completed, &mut start_id);
    // Match the transcript's message gutter. The disclosure control supplies
    // the remaining inline inset, so hosts must not add their own row padding.
    crate::message_row(threadlane_protocol::daemon::MessageRole::Assistant)
        .flex_none()
        .my_1()
        .px_4()
        .children((hidden_count > 0).then(|| {
            let label = format!("Completed · {summary}");
            let description = format!(
                "{}: {summary}",
                activities_disclosure_a11y(hidden_count, expanded)
            );
            crate::disclosure_button(
                SharedString::from(format!("activity-group-{id}")),
                expanded,
                description,
                div()
                    .debug_selector(|| "activity-group-label".into())
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::CircleCheck)
                            .xsmall()
                            .text_color(theme.success),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    ),
            )
            .debug_selector(|| "activity-group-disclosure".into())
            .when(expanded, |button| button.bg(theme.muted.opacity(0.25)))
            .xsmall()
            .on_click(move |_, window, cx| on_toggle(window, cx))
        }))
        .children(rows)
        .into_any_element()
}

fn activities_disclosure_a11y(count: usize, expanded: bool) -> String {
    format!(
        "{} {count} completed tool {}",
        if expanded { "Collapse" } else { "Expand" },
        if count == 1 { "activity" } else { "activities" }
    )
}

pub fn tool_activity(
    activity: &threadlane_protocol::daemon::ToolActivityInfo,
    has_detail: bool,
    detail: Option<AnyElement>,
    touch: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme().colors;
    let (status, status_color) = match activity.category.as_str() {
        "Error" => ("Failed", theme.danger),
        "Working" => ("Running", theme.info),
        "Thinking" => ("Thinking", theme.info),
        "Completed" | "Result" | "Edited" | "Created" | "Ran" | "Loaded" | "Explored" => {
            ("Completed", theme.muted_foreground)
        }
        other => (other, theme.muted_foreground),
    };
    let row_id = SharedString::from(activity.id.clone());
    let display_summary = activity.display_summary.clone();
    let disclosure_label = if has_detail {
        format!(
            "{} {}, {}",
            if activity.is_expanded {
                "Collapse"
            } else {
                "Expand"
            },
            display_summary,
            status
        )
    } else {
        format!("{display_summary}, {status}")
    };
    div()
        .debug_selector({
            let id = activity.id.clone();
            move || format!("tool-row-{id}").into()
        })
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .py_1()
        .child(
            Button::new(row_id)
                .debug_selector(|| "tool-activity-disclosure".into())
                .accessibility_label(disclosure_label.clone())
                .tooltip(disclosure_label)
                .ghost()
                .open(has_detail && activity.is_expanded)
                .when(has_detail && activity.is_expanded, |button| {
                    button.bg(theme.muted.opacity(0.25))
                })
                .small()
                .when(touch, |button| button.h_11())
                .w_full()
                .justify_start()
                .disabled(!has_detail)
                .gap_2()
                .when(has_detail, |row| {
                    row.on_click(move |_, window, cx| on_toggle(window, cx))
                })
                .child(
                    Icon::new(tool_icon(&activity.title))
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .children(
                            activity
                                .display_summary
                                .split_once(" · ")
                                .map(|(verb, target)| {
                                    (
                                        div()
                                            .flex_none()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(verb.to_owned()),
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .rounded_md()
                                            .px_1p5()
                                            .py_0p5()
                                            .bg(theme.muted.opacity(0.4))
                                            .text_xs()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_color(theme.muted_foreground)
                                            .child(target.to_owned()),
                                    )
                                })
                                .map(|(verb, target)| vec![verb, target])
                                .unwrap_or_else(|| {
                                    vec![div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_sm()
                                        .text_color(theme.foreground)
                                        .child(display_summary.clone())]
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_xs()
                        .text_color(status_color)
                        .child(match status {
                            "Running" | "Thinking" => gpui_component::spinner::Spinner::new()
                                .xsmall()
                                .color(status_color)
                                .into_any_element(),
                            "Completed" => Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(status_color)
                                .into_any_element(),
                            _ => div()
                                .flex_none()
                                .size_1()
                                .rounded_full()
                                .bg(status_color)
                                .into_any_element(),
                        })
                        .when(status != "Completed", |el| el.child(status.to_owned())),
                )
                .children(has_detail.then(|| {
                    crate::motion::chevron(
                        SharedString::from(format!("tool-{}", activity.id)),
                        activity.is_expanded,
                    )
                })),
        )
        .children(detail.map(|body| div().ml_6().mr_2().mt_1().min_w_0().child(body)))
        .into_any_element()
}

fn tool_icon(title: &str) -> IconName {
    let name = title.trim().to_lowercase().replace(' ', "_");
    match name.as_str() {
        "read_file" | "view_file" | "write_file" | "edit_file_hashline" | "apply_patch" => {
            IconName::File
        }
        "grep_search" | "find_by_name" | "search" => IconName::Search,
        "list_dir" => IconName::Folder,
        "run_command"
        | "run_terminal_command"
        | "execute_command"
        | "shell_command"
        | "bash"
        | "shell"
        | "terminal" => IconName::SquareTerminal,
        _ if name.starts_with("run_") || name.starts_with("execute_") => IconName::SquareTerminal,
        _ => IconName::Settings,
    }
}

#[cfg(test)]
mod tests {
    use super::activities_disclosure_a11y;

    #[test]
    fn completed_activity_labels_use_singular_for_one() {
        assert_eq!(
            activities_disclosure_a11y(1, false),
            "Expand 1 completed tool activity"
        );
        assert_eq!(
            activities_disclosure_a11y(3, false),
            "Expand 3 completed tool activities"
        );
        assert_eq!(
            activities_disclosure_a11y(1, true),
            "Collapse 1 completed tool activity"
        );
        assert_eq!(
            activities_disclosure_a11y(3, true),
            "Collapse 3 completed tool activities"
        );
    }
}

pub fn tool_group_summary<'a>(
    tools: impl IntoIterator<Item = &'a threadlane_protocol::daemon::ToolActivityInfo>,
) -> String {
    let mut counts = [0_usize; 6];
    for tool in tools {
        let kind = if crate::tool_detail::is_command_tool(&tool.title) {
            4
        } else {
            match tool.title.as_str() {
                "read_file" | "view_file" => 0,
                "grep_search" | "find_by_name" => 1,
                "list_dir" => 2,
                "write_file"
                | "write_to_file"
                | "edit_file_hashline"
                | "edit_files_hashline"
                | "replace_file_content"
                | "apply_diff"
                | "apply_patch"
                | "edit_file" => 3,
                _ => 5,
            }
        };
        counts[kind] += 1;
    }
    let labels = [
        ("read", "reads"),
        ("search", "searches"),
        ("listing", "listings"),
        ("edit", "edits"),
        ("command", "commands"),
        ("tool", "tools"),
    ];
    let parts = counts
        .into_iter()
        .zip(labels)
        .filter(|(count, _)| *count > 0)
        .map(|(count, (one, many))| format!("{count} {}", if count == 1 { one } else { many }))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "0 tools".into()
    } else {
        parts.join(" · ")
    }
}
