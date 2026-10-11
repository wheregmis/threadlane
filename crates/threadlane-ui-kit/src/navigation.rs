use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt};
use threadlane_protocol::daemon::SessionAttention;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SidebarDestination {
    Automations,
    Issues,
    PullRequests,
}

/// Controlled primary navigation. The host routes these destinations.
pub fn sidebar_navigation(
    selected: Option<SidebarDestination>,
    attention_count: usize,
    open_prs: usize,
    on_navigate: impl Fn(SidebarDestination, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let callback = std::rc::Rc::new(on_navigate);
    let theme = cx.theme().colors;
    let row = |id: &'static str,
               label: &'static str,
               icon: Icon,
               destination: SidebarDestination,
               count: usize,
               warning: bool,
               accessible: String| {
        let callback = callback.clone();
        div()
            .w_full()
            .p_1()
            .flex()
            .items_center()
            .child(
                Button::new(id)
                    .debug_selector(move || id.into())
                    .accessibility_label(accessible)
                    .tooltip(label)
                    .ghost()
                    .xsmall()
                    .compact()
                    .w_full()
                    .selected(selected == Some(destination))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_start()
                            .gap_2()
                            .w_full()
                            .px_1()
                            .child(icon.size_3p5().text_color(theme.foreground))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(label),
                            )
                            .children((count > 0).then(|| {
                                div()
                                    .px_1()
                                    .py(rems(0.03125))
                                    .rounded_full()
                                    .bg(if warning {
                                        theme.warning
                                    } else {
                                        theme.primary
                                    }
                                    .opacity(0.2))
                                    .text_xs()
                                    .font_bold()
                                    .text_color(if warning {
                                        theme.warning
                                    } else {
                                        theme.primary
                                    })
                                    .child(count.to_string())
                            })),
                    )
                    .on_click(move |_, window, cx| callback(destination, window, cx)),
            )
    };
    let current_label = |destination, label: &str| {
        if selected == Some(destination) {
            format!("{label}, current view")
        } else {
            label.into()
        }
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(row(
            "sidebar-automations",
            "Automations",
            Icon::new(IconName::Calendar),
            SidebarDestination::Automations,
            attention_count,
            true,
            current_label(
                SidebarDestination::Automations,
                &format!("Automations, {attention_count} runs need attention"),
            ),
        ))
        .child(row(
            "sidebar-issues",
            "Issues",
            Icon::default().path("icons/git/issue.svg"),
            SidebarDestination::Issues,
            0,
            false,
            current_label(SidebarDestination::Issues, "Open GitHub issues"),
        ))
        .child(row(
            "sidebar-pull-requests",
            "PRs",
            Icon::default().path("icons/git/pull-request.svg"),
            SidebarDestination::PullRequests,
            open_prs,
            false,
            current_label(
                SidebarDestination::PullRequests,
                "Open GitHub pull requests",
            ),
        ))
}

pub fn sidebar_footer_surface(cx: &App) -> Div {
    let theme = cx.theme().colors;
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(theme.border.opacity(0.25))
        .bg(theme.title_bar)
}

pub fn sidebar_settings_button(selected: bool, cx: &App) -> Button {
    let theme = cx.theme().colors;
    Button::new("sidebar-settings")
        .debug_selector(|| "sidebar-settings".into())
        .accessibility_label("Open settings")
        .tooltip("Open settings")
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_start()
                .gap_2()
                .child(Icon::new(IconName::Settings).size_4())
                .child("Settings"),
        )
        .ghost()
        .selected(selected)
        .flex_1()
        .min_w_0()
        .justify_start()
        .text_color(if selected {
            theme.foreground
        } else {
            theme.muted_foreground
        })
}

pub fn sidebar_pairing_button(cx: &App) -> Button {
    Button::new("sidebar-pair-device")
        .accessibility_label("Share with mobile")
        .tooltip("Share with mobile")
        .ghost()
        .child(
            Icon::default()
                .path("icons/smartphone.svg")
                .size_4()
                .text_color(cx.theme().muted_foreground),
        )
}

pub fn session_card_content() -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1p5()
        .pl_3p5()
        .pr_3()
        .py_2p5()
}

pub fn session_card_title_row() -> Div {
    div().flex().items_center().justify_between().gap_2()
}

pub fn session_context_row() -> Div {
    div().flex().items_center().gap_2().text_xs().min_w_0()
}

pub fn session_signal_row() -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .text_xs()
        .min_w_0()
        .flex_wrap()
}

pub fn sidebar_project_filter_row() -> Div {
    div().flex().items_center().gap_1().pl_4().pr_3().pt_2().pb_1()
}

pub fn sidebar_project_filter_button(label: &str, selected: bool, cx: &App) -> Button {
    let theme = cx.theme().colors;
    // A quiet filter control, not a destination: it scopes the task list
    // below and must not look like Automations/Issues/PRs above it.
    Button::new("sidebar-project-filter")
        .debug_selector(|| "sidebar-project-filter".into())
        .accessibility_label(format!("Filter tasks by project: {label}"))
        .tooltip("Filter tasks by project")
        .dropdown_caret(true)
        .selected(selected)
        .ghost()
        .xsmall()
        .child(
            div()
                .min_w_0()
                .max_w(rems(9.0))
                .truncate()
                .text_xs()
                .text_color(if selected {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .child(label.to_owned()),
        )
}

/// Controlled sidebar filter. Hosts build choices and own filtering/attachment.
pub fn sidebar_project_filter(
    label: &str,
    selected: bool,
    menu: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    attach: Button,
    cx: &App,
) -> Div {
    sidebar_project_filter_row()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child("Projects"),
        )
        .child(
            div()
                .min_w_0()
                .flex_none()
                .child(sidebar_project_filter_button(label, selected, cx).dropdown_menu(menu)),
        )
        .child(attach)
        .bg(cx.theme().title_bar)
}

pub fn sidebar_project_filter_item(label: &str, sessions: usize, selected: bool) -> PopupMenuItem {
    crate::project_picker_item(format!("{label} · {sessions}"), selected)
}

pub fn sidebar_attach_project_button() -> Button {
    // A folder-plus, not a bare plus: a lone plus in the sidebar reads as
    // "new task", which is the separate button above.
    Button::new("attach-project-btn")
        .icon(Icon::default().path("icons/folder-plus.svg"))
        .accessibility_label("Attach project folder")
        .tooltip("Attach a project folder…")
        .ghost()
        .xsmall()
}

pub fn session_project_label(project: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme().colors;
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap_1()
        .text_color(theme.muted_foreground.opacity(0.85))
        .child(
            Icon::new(IconName::Folder)
                .xsmall()
                .text_color(theme.muted_foreground.opacity(0.55)),
        )
        .child(div().min_w_0().truncate().child(project.into()))
}

pub fn session_branch_badge(
    session: &threadlane_protocol::daemon::SessionInfo,
    cx: &App,
) -> Option<AnyElement> {
    let theme = cx.theme().colors;
    if session.is_worktree && !session.worktree_available {
        let branch = session.git_branch.as_deref().unwrap_or("worktree");
        let tooltip = format!("Worktree unavailable\nBranch: '{branch}'\nNot checked out locally\nRecorded path: {}\nSession history remains available", session.runtime_work_dir.display());
        return Some(Button::new(SharedString::from(format!("session-worktree-{}", session.id)))
            .icon(Icon::default().path("icons/git/branch.svg")).label("Not checked out")
            .accessibility_label(tooltip.clone())
            .tooltip(tooltip)
            .ghost().xsmall().bg(theme.warning.opacity(0.12)).rounded_full().text_color(theme.warning).into_any_element());
    }
    session.git_branch.as_ref().map(|branch| {
        let tooltip = format!("Branch: {branch}");
        div()
            .id(SharedString::from(format!(
                "session-branch-badge-{}",
                session.id
            )))
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_1p5()
            .py(rems(0.125))
            .rounded_full()
            .bg(theme.muted.opacity(0.3))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .child(
                Icon::default()
                    .path("icons/git/branch.svg")
                    .size(rems(0.6875))
                    .text_color(theme.muted_foreground.opacity(0.85)),
            )
            .child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(theme.muted_foreground)
                    .max_w(rems(5.5))
                    .truncate()
                    .child(branch.clone()),
            )
            .into_any_element()
    })
}

/// Sidebar geometry; the host supplies navigation, filters, a retained list and footer.
pub fn sidebar_surface(cx: &App) -> Div {
    div()
        .debug_selector(|| "workspace-sidebar".into())
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(cx.theme().title_bar)
}

/// Production brand, new-task control and activity indicator, shared by every host.
///
/// `toggle_row` replaces the brand row where the app draws a caption strip
/// (the brand moves into the strip): the row then only reserves room for the
/// floating sidebar toggle and keeps the activity indicator.
pub fn sidebar_header(
    loading: bool,
    working: bool,
    attention: SessionAttention,
    navigation: AnyElement,
    toggle_row: bool,
    on_new_task: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    // The toggle floats over this row, so it needs no leading content.
    let leading = if toggle_row {
        div().flex_1().into_any_element()
    } else {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size(rems(1.5))
                    .rounded_lg()
                    .bg(theme.foreground.opacity(0.12))
                    .border_1()
                    .border_color(theme.foreground.opacity(0.2))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::default()
                            .path("icons/threadlane.svg")
                            .size_4()
                            .text_color(theme.foreground),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child("Threadlane"),
            )
            .into_any_element()
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px_2p5()
        .when(!toggle_row, |header| {
            header.pt(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
        })
        .pb_1p5()
        .bg(theme.title_bar)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px_1p5()
                .map(|row| {
                    if toggle_row {
                        // Same height as the other panel headers; the left
                        // padding clears the floating sidebar toggle.
                        row.min_h(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
                            .pl(rems(threadlane_ui_theme::WINDOW_CONTROLS_CONTENT_INSET - 1.0))
                    } else {
                        row.pt_1().pb_2()
                    }
                })
                .child(leading)
                .child({
                    let is_loading = loading;
                    let active_attention = Some(attention);
                    let is_generating =
                        working || active_attention == Some(SessionAttention::Working);

                    // The header stays clean when idle: Ready is already
                    // visible on the session card and chat header, so only
                    // Loading / Working / Needs-you surface here.
                    if is_loading {
                        div()
                            .flex()
                            .items_center()
                            .gap(rems(0.25))
                            .px_2()
                            .py(rems(0.0625))
                            .rounded_full()
                            .bg(theme.muted.opacity(0.35))
                            .border_1()
                            .border_color(theme.border.opacity(0.2))
                            .child(Spinner::new().xsmall().color(theme.muted_foreground))
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(theme.muted_foreground)
                                    .child("Loading"),
                            )
                            .into_any_element()
                    } else if is_generating {
                        div()
                            .id("sidebar-working-indicator")
                            .flex()
                            .items_center()
                            .gap(rems(0.25))
                            .px_2()
                            .py(rems(0.0625))
                            .rounded_full()
                            .bg(theme.info.opacity(0.12))
                            .border_1()
                            .border_color(theme.info.opacity(0.25))
                            .child(Spinner::new().xsmall().color(theme.info))
                            .into_any_element()
                    } else if active_attention == Some(SessionAttention::NeedsYou) {
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .py(rems(0.0625))
                            .rounded_full()
                            .bg(theme.warning.opacity(0.15))
                            .border_1()
                            .border_color(theme.warning.opacity(0.28))
                            .child(div().size(rems(0.375)).rounded_full().bg(theme.warning))
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(theme.warning)
                                    .child("Needs you"),
                            )
                            .into_any_element()
                    } else {
                        Empty.into_any_element()
                    }
                }),
        )
        .child(
            div()
                .w_full()
                .p_1()
                .rounded_xl()
                .bg(theme.muted.opacity(0.28))
                .flex()
                .items_center()
                .child(
                    Button::new("new-task-btn")
                        .accessibility_label(new_task_tooltip())
                        .ghost()
                        .xsmall()
                        .compact()
                        .w_full()
                        .justify_start()
                        .tooltip(new_task_tooltip())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_start()
                                .gap_2()
                                .w_full()
                                .px_1()
                                .child(
                                    Icon::new(IconName::Plus)
                                        .size_3p5()
                                        .text_color(theme.primary),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .child("New task"),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .px_1p5()
                                        .py(rems(0.125))
                                        .rounded_md()
                                        .bg(theme.muted.opacity(0.5))
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.muted_foreground.opacity(0.85))
                                        .child(new_task_shortcut()),
                                ),
                        )
                        .on_click(move |_event, window, cx| {
                            on_new_task(window, cx);
                        }),
                ),
        )
        .child(navigation)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateGroup {
    Pinned,
    NeedsYou,
    Working,
    Today,
    Yesterday,
    ThisWeek,
    Older,
    /// Confirmed snoozes live below every date group, ordered by return
    /// time. Pending (unconfirmed) snoozes stay in their normal group.
    Snoozed,
}

impl DateGroup {
    pub const COUNT: usize = 8;

    pub fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This Week",
            Self::Older => "Older",
            Self::Snoozed => "Snoozed",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Pinned => 0,
            Self::NeedsYou => 1,
            Self::Working => 2,
            Self::Today => 3,
            Self::Yesterday => 4,
            Self::ThisWeek => 5,
            Self::Older => 6,
            Self::Snoozed => 7,
        }
    }
}

/// Callers pass a shared `now` so a render pass performs one clock read
/// instead of one per row.
pub fn session_date_group(timestamp: u64, now: u64) -> DateGroup {
    let seconds = now.saturating_sub(timestamp);
    if seconds < 86400 {
        DateGroup::Today
    } else if seconds < 172800 {
        DateGroup::Yesterday
    } else if seconds < 604800 {
        DateGroup::ThisWeek
    } else {
        DateGroup::Older
    }
}

pub fn session_title_text(title: impl Into<SharedString>, active: bool, cx: &App) -> Div {
    div()
        .w_full()
        .min_w_0()
        .text_sm()
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .text_color(if active {
            cx.theme().foreground
        } else {
            cx.theme().sidebar_foreground
        })
        .truncate()
        .child(title.into())
}

pub fn session_group_header(group: DateGroup, first: bool, window: &Window, cx: &App) -> Div {
    let theme = cx.theme().colors;
    let status_dot = match group {
        DateGroup::Pinned => Some(
            div().flex().items_center().child(
                Icon::default()
                    .path("icons/pin.svg")
                    .size(rems(0.6875))
                    .text_color(theme.primary),
            ),
        ),
        DateGroup::NeedsYou => Some(div().size(rems(0.4375)).rounded_full().bg(theme.warning)),
        DateGroup::Working => Some(div().size(rems(0.4375)).rounded_full().bg(theme.primary)),
        _ => None,
    };

    let is_pinned_group = group == DateGroup::Pinned;
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .pt(if first {
            window.rem_size() * 0.25
        } else {
            window.rem_size() * 0.75
        })
        .pb_1()
        .children(status_dot)
        .child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(if is_pinned_group {
                    theme.primary
                } else {
                    theme.muted_foreground.opacity(0.85)
                })
                .child(group.label()),
        )
        .child(div().h(rems(0.0625)).flex_1().bg(if is_pinned_group {
            theme.primary.opacity(0.25)
        } else {
            theme.border.opacity(0.25)
        }))
}

/// A controlled section header; collapse never changes scheduling or session state.
pub fn sidebar_snoozed_header(
    count: usize,
    collapsed: bool,
    first: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let chevron = if collapsed {
        IconName::ChevronRight
    } else {
        IconName::ChevronDown
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .pt(if first {
            window.rem_size() * 0.25
        } else {
            window.rem_size() * 0.75
        })
        .pb_1()
        .child(
            Icon::new(IconName::Moon)
                .xsmall()
                .text_color(theme.muted_foreground.opacity(0.85)),
        )
        .child(
            Button::new("snoozed-section-toggle")
                .debug_selector(|| "snoozed-section-toggle".into())
                .icon(Icon::new(chevron))
                .label(format!("Snoozed ({count})"))
                .accessibility_label(if collapsed {
                    format!("Snoozed, {count} sessions, collapsed")
                } else {
                    format!("Snoozed, {count} sessions, expanded")
                })
                .tooltip(if collapsed {
                    "Expand snoozed sessions"
                } else {
                    "Collapse snoozed sessions"
                })
                .ghost()
                .xsmall()
                .compact()
                .text_color(theme.muted_foreground.opacity(0.85))
                .on_click(move |_, window, cx| on_toggle(window, cx)),
        )
        .child(
            div()
                .h(rems(0.0625))
                .flex_1()
                .bg(theme.border.opacity(0.25)),
        )
}

#[derive(Clone, Copy)]
pub enum SidebarEmptyAction {
    ClearFilters,
    NewTask,
}

pub fn sidebar_history_empty(
    has_filters: bool,
    on_action: impl Fn(SidebarEmptyAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let request = std::rc::Rc::new(on_action);
    div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .px_4()
        .py_8()
        .child(
            div()
                .size(rems(2.5))
                .rounded_full()
                .bg(theme.muted.opacity(0.4))
                .border_1()
                .border_color(theme.border.opacity(0.25))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(if has_filters {
                        IconName::Search
                    } else {
                        IconName::SquareTerminal
                    })
                    .small()
                    .text_color(theme.muted_foreground.opacity(0.7)),
                ),
        )
        .child(
            // Single-line copy: the retained text measurement can paint a
            // wrap taken in an earlier, narrower pass inside one reserved
            // line, drawing the tail over the button. Copy fits the 12rem
            // minimum sidebar; the ellipsis only guards larger fonts.
            div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(if has_filters {
                            "No matching tasks"
                        } else {
                            "No tasks yet"
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .text_center()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .overflow_hidden()
                        .child(if has_filters {
                            "Try a different project"
                        } else {
                            "Start a task to begin coding"
                        }),
                ),
        )
        .when(has_filters, |this| {
            this.child(
                Button::new("empty-history-clear-filters")
                    .debug_selector(|| "empty-history-clear-filters".into())
                    .label("Clear filter")
                    .tooltip("Clear project filter")
                    .outline()
                    .small()
                    .on_click({
                        let request = request.clone();
                        move |_, window, cx| request(SidebarEmptyAction::ClearFilters, window, cx)
                    }),
            )
        })
        .children((!has_filters).then(|| {
            Button::new("empty-history-new-task")
                .icon(IconName::Plus)
                .label("New task")
                .outline()
                .small()
                .accessibility_label("Start a new task")
                .tooltip(new_task_tooltip())
                .on_click(move |_, window, cx| request(SidebarEmptyAction::NewTask, window, cx))
        }))
}

/// Shared precedence for ordinary rows. Confirmed snoozes are grouped separately
/// by the host, while pending saves retain this original group.
pub fn session_history_group(
    pinned: bool,
    attention: SessionAttention,
    timestamp: u64,
    now: u64,
) -> DateGroup {
    if pinned {
        DateGroup::Pinned
    } else {
        match attention {
            SessionAttention::NeedsYou => DateGroup::NeedsYou,
            SessionAttention::Working => DateGroup::Working,
            SessionAttention::Ready | SessionAttention::Idle => session_date_group(timestamp, now),
        }
    }
}

/// Shared card inset; group headers own their own alignment spine.
pub fn session_history_card_row(card: impl IntoElement) -> Div {
    div().px_2().child(card)
}


/// Platform-appropriate label for the `BeginNewTask` binding (cmd-n on macOS,
/// ctrl-n elsewhere).
pub fn new_task_shortcut() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘N"
    } else {
        "Ctrl+N"
    }
}

fn new_task_tooltip() -> String {
    format!("Start a new task ({})", new_task_shortcut())
}

/// Path text for display. Strips the Windows verbatim prefix (`\\?\C:\…`)
/// that canonicalization adds; it is noise to a reader.
pub fn display_path(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => text,
    }
}

#[cfg(test)]
mod display_tests {
    use super::display_path;
    use std::path::Path;

    #[test]
    fn display_path_strips_only_drive_verbatim_prefix() {
        assert_eq!(display_path(Path::new(r"\\?\C:\work\repo")), r"C:\work\repo");
        assert_eq!(display_path(Path::new(r"\\?\UNC\srv\share")), r"\\?\UNC\srv\share");
        assert_eq!(display_path(Path::new("/home/me/repo")), "/home/me/repo");
    }
}
