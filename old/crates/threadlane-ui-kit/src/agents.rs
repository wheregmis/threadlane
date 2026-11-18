//! Controlled agent profiles and activity. Hosts own selection, transcripts and services.
use gpui::{prelude::*, *};
use gpui_component::avatar::Avatar;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};
use threadlane_protocol::daemon::{
    ChatMessageInfo, MessageRole, SubagentActivityStatus, ToolActivityInfo,
};

pub fn agent_status_label(status: SubagentActivityStatus) -> &'static str {
    match status {
        SubagentActivityStatus::Queued => "Queued",
        SubagentActivityStatus::Running => "Working",
        SubagentActivityStatus::Completed => "Completed",
        SubagentActivityStatus::Failed => "Failed",
        SubagentActivityStatus::Cancelled => "Cancelled",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentWorktreeAction {
    Inspect,
    Terminal,
    Apply,
    Discard,
}

/// Presentation eligibility only; hosts revalidate the owning lane and checkout.
pub fn agent_worktree_action_enabled(
    action: AgentWorktreeAction,
    status: SubagentActivityStatus,
    available: bool,
) -> bool {
    match action {
        AgentWorktreeAction::Inspect => true,
        AgentWorktreeAction::Terminal => available,
        AgentWorktreeAction::Apply => status == SubagentActivityStatus::Completed,
        AgentWorktreeAction::Discard => !matches!(
            status,
            SubagentActivityStatus::Queued | SubagentActivityStatus::Running
        ),
    }
}

/// Controlled branch actions over canonical isolation metadata. Never probes the filesystem.
pub fn agent_worktree_controls(
    id: &str,
    isolation: &threadlane_protocol::events::SubagentIsolation,
    status: SubagentActivityStatus,
    available: bool,
    on_action: impl Fn(AgentWorktreeAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let colors = cx.theme().colors;
    let branch = isolation.branch.clone();
    let path = isolation.workspace.display().to_string();
    let on_action = std::rc::Rc::new(on_action);
    let context = format!("Agent branch: {branch}\nWorktree: {path}");
    let tooltip = context.clone();
    let branch_selector = format!("agent-branch-{id}");
    let buttons = [
        (
            AgentWorktreeAction::Inspect,
            "inspect",
            "Inspect diff",
            format!("Inspect committed changes on {branch}"),
        ),
        (
            AgentWorktreeAction::Terminal,
            "terminal",
            "Terminal",
            if available {
                format!("Open terminal in {path}")
            } else {
                format!("Worktree unavailable at {path}; branch {branch} is still available.")
            },
        ),
        (
            AgentWorktreeAction::Apply,
            "apply",
            "Apply",
            if status == SubagentActivityStatus::Completed {
                format!("Merge committed changes from {branch} into the parent checkout, then remove its worktree and branch.")
            } else {
                "Only completed agent work can be applied.".into()
            },
        ),
        (
            AgentWorktreeAction::Discard,
            "discard",
            "Discard…",
            if matches!(
                status,
                SubagentActivityStatus::Queued | SubagentActivityStatus::Running
            ) {
                "Wait for this agent to stop before discarding its worktree.".into()
            } else {
                format!("Discard local branch {branch} and its worktree…")
            },
        ),
    ];
    div()
        .debug_selector(|| "agent-worktree-controls".into())
        .mx_3()
        .mb_2()
        .p_2p5()
        .rounded_xl()
        .border_1()
        .border_color(colors.border)
        .bg(colors.muted.opacity(0.2))
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .text_xs()
                .text_color(colors.muted_foreground)
                .child(Icon::default().path("icons/git/branch.svg").xsmall())
                .child(
                    div()
                        .id(SharedString::from(format!("agent-branch-{id}")))
                        .role(Role::Label)
                        .aria_label(context)
                        .debug_selector(move || branch_selector.clone())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(branch)
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1()
                .flex_wrap()
                .children(buttons.into_iter().map(|(action, suffix, label, tooltip)| {
                    let selector = format!("agent-{suffix}-{id}");
                    let mut button = Button::new(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .label(label)
                        .xsmall()
                        .disabled(!agent_worktree_action_enabled(action, status, available))
                        .accessibility_label(tooltip.clone())
                        .tooltip(tooltip);
                    if action == AgentWorktreeAction::Inspect {
                        button = button.outline();
                    }
                    if matches!(
                        action,
                        AgentWorktreeAction::Terminal | AgentWorktreeAction::Discard
                    ) {
                        button = button.ghost();
                    }
                    let on_action = on_action.clone();
                    button.on_click(move |_, window, cx| on_action(action, window, cx))
                })),
        )
}

/// Shared confirmation copy and destructive treatment. The host supplies guarded on-ok work.
pub fn agent_worktree_discard_dialog(
    alert: gpui_component::dialog::AlertDialog,
    isolation: &threadlane_protocol::events::SubagentIsolation,
    parent: &std::path::Path,
) -> gpui_component::dialog::AlertDialog {
    use gpui_component::{button::ButtonVariant, dialog::DialogButtonProps};
    alert.title(format!("Discard branch “{}”?",isolation.branch))
        .description(format!("Delete this local branch and its isolated worktree at {}. Uncommitted files in that worktree and unmerged commits will be permanently lost.\nThe parent checkout at {} and the saved agent transcript will be kept.",isolation.workspace.display(),parent.display()))
        .button_props(DialogButtonProps::default().ok_text("Discard").ok_variant(ButtonVariant::Danger).show_cancel(true))
}

fn status_color(status: SubagentActivityStatus, cx: &App) -> Hsla {
    let colors = cx.theme().colors;
    match status {
        SubagentActivityStatus::Running => colors.success,
        SubagentActivityStatus::Queued => colors.warning,
        SubagentActivityStatus::Failed => colors.danger,
        SubagentActivityStatus::Cancelled | SubagentActivityStatus::Completed => {
            colors.muted_foreground
        }
    }
}

pub fn agent_status_pill(status: SubagentActivityStatus, cx: &App) -> Div {
    let color = status_color(status, cx);
    div()
        .rounded_full()
        .px_2()
        .py_0p5()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .bg(color.opacity(0.14))
        .text_color(color)
        .child(agent_status_label(status))
}

pub fn agent_panel_surface(cx: &App) -> Div {
    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(cx.theme().background)
}

/// Horizontal profile strip; callers append controlled profile buttons.
pub fn agent_profile_tabs(cx: &App) -> gpui_component::scroll::Scrollable<Div> {
    let theme = cx.theme().colors;
    div()
        .debug_selector(|| "agent-profile-tabs".into())
        .flex()
        .flex_none()
        .items_start()
        .gap_1()
        .px_3()
        .pt_2()
        .pb_2()
        .h(rems(5.5))
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.title_bar.opacity(0.35))
        .overflow_x_scrollbar()
}

pub fn agent_profile_button(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    description: impl Into<SharedString>,
    status: SubagentActivityStatus,
    selected: bool,
    cx: &App,
) -> Button {
    let theme = cx.theme().colors;
    let name = name.into();
    let description = description.into();
    Button::new(id)
        .ghost()
        .h(rems(3.5))
        .selected(selected)
        .tooltip(description.clone())
        .accessibility_label(description)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1()
                .min_w(rems(4.0))
                .child(
                    div()
                        .relative()
                        .child(Avatar::new().name(name.clone()).small())
                        .child(
                            div()
                                .absolute()
                                .bottom_0()
                                .right_0()
                                .size(rems(0.625))
                                .rounded_full()
                                .border_2()
                                .border_color(theme.title_bar)
                                .bg(status_color(status, cx)),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_center()
                        .max_w(rems(6.0))
                        .truncate()
                        .font_weight(if selected {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(if selected {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(name),
                ),
        )
}

pub fn agent_main_summary(working: bool, latest: Option<String>, cx: &App) -> Div {
    let theme = cx.theme().colors;
    let color = if working {
        theme.primary
    } else {
        theme.muted_foreground
    };
    div()
        .mx_3()
        .mt_3()
        .p_3()
        .rounded_xl()
        .border_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::Bot).small())
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_sm()
                        .child("Main agent"),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .rounded_full()
                        .px_2()
                        .py_0p5()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .bg(color.opacity(0.14))
                        .text_color(color)
                        .child(if working { "Working" } else { "Ready" }),
                ),
        )
        .children(latest.map(|text| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        }))
}

/// Header and instruction; the host supplies the Message/Continue command.
pub fn agent_detail_header(
    name: String,
    status: SubagentActivityStatus,
    task: String,
    action: impl IntoElement,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    div()
        .debug_selector(|| "agent-detail-header".into())
        .flex_none()
        .min_w_0()
        .px_3()
        .pt_3()
        .pb_2()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_sm()
                        .child(name),
                )
                .child(agent_status_pill(status, cx))
                .child(action),
        )
        .child(
            div()
                .p_2p5()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted.opacity(0.3))
                .text_sm()
                .text_color(theme.foreground)
                .whitespace_normal()
                .child(task),
        )
}

pub fn agent_detail_surface(cx: &App) -> Div {
    div()
        .debug_selector(|| "agent-detail".into())
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(cx.theme().border)
}

pub fn agent_empty_state(main: bool, cx: &App) -> Div {
    let theme = cx.theme().colors;
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .text_center()
        .child(
            Icon::new(IconName::Bot)
                .large()
                .text_color(theme.muted_foreground.opacity(0.6)),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                // The selected agent's card above already names it.
                .child("No activity yet"),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground.opacity(0.8))
                .child(if main {
                    "Tool calls and replies appear here once the agent runs."
                } else {
                    "The prompt is shown above. New tool calls and replies will appear here."
                }),
        )
}

/// Virtual rows can be measured outside the Root's inherited text style.
/// Bind the current interface font at their shared presentation boundary.
pub fn agent_activity_row(cx: &App) -> Div {
    div()
        .px_3()
        .py_2()
        .font_family(cx.theme().font_family.clone())
}

pub fn agent_error_row(error: Option<String>, cx: &App) -> Div {
    div()
        .p_3()
        .font_family(cx.theme().font_family.clone())
        .children(error.map(|error| {
            div()
                .id("agent-activity-error")
                .role(Role::Alert)
                .aria_label(error.clone())
                .p_2()
                .rounded_lg()
                .text_color(cx.theme().danger)
                .child(error)
        }))
}

pub fn agent_activity_message(message: &ChatMessageInfo, tools: Vec<AnyElement>, cx: &App) -> Div {
    let theme = cx.theme().colors;
    let role = match message.role {
        MessageRole::User => "Instruction",
        MessageRole::Assistant => "Agent",
        MessageRole::System => "System",
        MessageRole::Error => "Error",
        MessageRole::ContextMarker => "Context",
    };
    div()
        .debug_selector(|| "agent-activity-message".into())
        .font_family(cx.theme().font_family.clone())
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_xl()
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted.opacity(0.3))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if message.role == MessageRole::Error {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(role),
        )
        .children((!message.content.trim().is_empty()).then(|| {
            div()
                .text_sm()
                .text_color(theme.foreground)
                .child(message.content.clone())
        }))
        .children(
            message
                .reasoning_content
                .as_ref()
                .filter(|text| !text.trim().is_empty())
                .map(|text| {
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if message.streaming {
                            "Thinking…"
                        } else {
                            "Thought process"
                        })
                        .child(div().whitespace_normal().child(text.clone()))
                }),
        )
        .children(tools)
}

/// Uses the same lazy, animated tool disclosure and output cards as chat.
/// `id` must include the session, lane, message and tool identity.
/// These activity cards expose read-only file previews; navigation stays with the host.
/// Use the returned motion to remeasure the host's virtualized row during transitions.
pub fn agent_tool_activity(
    id: String,
    activity: &ToolActivityInfo,
    expanded: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> (AnyElement, crate::DisclosureMotion) {
    let mut activity = activity.clone();
    activity.id = id.clone();
    activity.is_expanded = expanded;
    if activity.display_summary.trim().is_empty() {
        activity.display_summary = activity.title.clone();
    }
    let motion = crate::DisclosureMotion::new(
        SharedString::from(format!("agent-tool-motion-{id}")),
        expanded,
        window,
        cx,
    );
    let detail = motion.is_visible().then(|| {
        let args = crate::tool_detail::args_json(&activity.arguments).unwrap_or_default();
        let path = threadlane_protocol::tool::read_file_snapshot_path(&activity.detail)
            .or_else(|| crate::tool_detail::args_path(&args))
            .unwrap_or_else(|| ".".into());
        let body = crate::tool_preview::render(
            &activity,
            path.clone(),
            path.into(),
            |id, path, line, folder| {
                crate::tool_preview::open_button(id, &path, line, folder, false, |_, _, _| {})
            },
            cx,
        )
        .or_else(|| {
            crate::tool_detail::render_activity_detail_card(
                &activity,
                None::<fn(String, &mut App)>,
                cx,
            )
        })
        .unwrap_or_else(|| {
            crate::result_surface(&cx.theme().colors)
                .child(
                    crate::result_header(&cx.theme().colors)
                        .text_xs()
                        .child("Output"),
                )
                .child(
                    crate::result_viewport(format!("agent-output-{id}")).child(
                        crate::result_scroll_body(format!("agent-output-scroll-{id}"), div()
                            .p_3()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(activity.detail.clone())),
                    ),
                )
                .into_any_element()
        });
        motion.content(body)
    });
    let row = crate::tool_activity(
        &activity,
        !activity.detail.trim().is_empty() || crate::tool_detail::expandable(&activity),
        detail,
        false,
        on_toggle,
        cx,
    );
    (row, motion)
}
