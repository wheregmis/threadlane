//! Complete controlled sidebar row. Hosts supply captured/live facts and command handlers.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, StyledExt};
use threadlane_protocol::daemon::{SessionAttention, SessionInfo};
use threadlane_protocol::repo::{GitHubPrInfo, GitStatus};

pub struct SidebarSessionCardState {
    pub project: String,
    /// False when only one project is attached: the label would repeat on
    /// every card. The tooltip and accessible label still name it.
    pub show_project: bool,
    pub attention: SessionAttention,
    pub selected: bool,
    pub pinned: bool,
    pub unseen_result: bool,
    pub snooze: Option<crate::SidebarSnoozeStatus>,
    pub git_status: Option<GitStatus>,
    pub pr: Option<GitHubPrInfo>,
    pub now: u64,
}

pub fn sidebar_session_card(
    session: &SessionInfo,
    state: SidebarSessionCardState,
    on_action: impl Fn(crate::SidebarSessionAction, &mut Window, &mut App) + 'static,
    quick_menu: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    full_menu: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    cx: &App,
) -> impl IntoElement {
    let attention = state.attention;
    let is_active = state.selected;
    let theme = cx.theme().colors;
    let status_indicator = crate::session_attention(&session.id, attention, cx);

    let session_identity = crate::session_identity(session);
    let session_title = session_identity.title;
    let time_ago = session_time_ago(session.updated_at, state.now);
    let project = state.project;
    let state_show_project = state.show_project;
    // Rich hover card (Synara ThreadHoverCardContent pattern): keep the
    // row to title + status, move project path, branch/worktree, recency,
    // and attention detail into the tooltip.
    let work_dir_display = session.work_dir.to_string_lossy().into_owned();
    let branch_display = session.git_branch.as_deref().unwrap_or("no branch");
    let worktree_display = if session.is_worktree {
        if session.worktree_available {
            "worktree"
        } else {
            "worktree unavailable"
        }
    } else {
        "local checkout"
    };
    let has_unseen_result = state.unseen_result;
    let session_tooltip = format!(
        "{}\n{} · {}\nBranch: {branch_display} ({worktree_display})\n{} · {}",
        session_identity.tooltip,
        project,
        work_dir_display,
        time_ago,
        attention.label(),
    );
    let session_tooltip = if has_unseen_result {
        format!("{session_tooltip}\nNew result: a finished run has output you have not seen yet")
    } else {
        session_tooltip
    };

    let is_pinned = state.pinned;
    let session_snooze = state.snooze;
    let snooze_label = session_snooze.as_ref().map(|snooze| snooze.label());
    let session_git_status = state.git_status;
    let request = std::rc::Rc::new(on_action);
    let select_row = request.clone();
    let select_title = request.clone();
    let pin = request.clone();
    let archive = request.clone();

    // Mirror the context on the focusable title, since keyboard and
    // screen-reader users activate the session through that button.
    let branch_suffix = session
        .git_branch
        .as_deref()
        .map(|branch| format!(", branch {branch}"))
        .unwrap_or_default();
    let pinned_prefix = if is_pinned { "Pinned, " } else { "" };
    let unseen_suffix = if has_unseen_result {
        ", new result"
    } else {
        ""
    };
    // Snooze and deadline state are mirrored into the row and title
    // labels — never a color-only cue.
    let snooze_suffix = snooze_label
        .as_ref()
        .map(|label| format!(", {}", label.to_lowercase()))
        .unwrap_or_default();
    let session_row_label = format!(
        "{pinned_prefix}{}, project {}, {}, {}{}{}{}",
        session_title,
        project,
        attention.label(),
        time_ago,
        branch_suffix,
        unseen_suffix,
        snooze_suffix,
    );

    let pr_info = state.pr;

    let pr_meta = pr_info.map(|pr| {
        let state_upper = pr.state.to_uppercase();
        let is_merged = state_upper == "MERGED";
        let is_draft = pr.is_draft || state_upper == "DRAFT";
        let is_closed = state_upper == "CLOSED";
        let tooltip = sidebar_pr_status_tooltip(&pr);

        let (pr_bg, pr_fg, pr_border, pr_label, pr_icon) = if is_merged {
            (
                theme.success.opacity(0.15),
                theme.success,
                theme.success.opacity(0.28),
                format!("#{}", pr.number),
                Icon::default().path("icons/git/branch.svg"),
            )
        } else if is_draft {
            (
                theme.secondary,
                theme.muted_foreground,
                theme.border.opacity(0.3),
                format!("#{}", pr.number),
                Icon::default().path("icons/git/compare.svg"),
            )
        } else if is_closed {
            (
                theme.danger.opacity(0.12),
                theme.danger,
                theme.danger.opacity(0.25),
                format!("#{}", pr.number),
                Icon::default().path("icons/git/compare.svg"),
            )
        } else {
            (
                theme.primary.opacity(0.12),
                theme.primary,
                theme.primary.opacity(0.25),
                format!("#{}", pr.number),
                Icon::default().path("icons/git/compare.svg"),
            )
        };

        div().flex().flex_none().items_center().gap_1().child(
            Button::new(SharedString::from(format!(
                "session-pr-{}-{}",
                session.id, pr.number
            )))
            .icon(pr_icon)
            .label(pr_label)
            .accessibility_label(tooltip.clone())
            .tooltip(tooltip)
            .ghost()
            .xsmall()
            .bg(pr_bg)
            .border_1()
            .border_color(pr_border)
            .rounded_full()
            .text_color(pr_fg),
        )
    });

    // Three-row card: title / context (where) / signals (what needs
    // attention). The old single wrapping meta row crammed project,
    // branch, git, PR, pinned, and status into one line with bullet
    // separators. Splitting keeps each row single-purpose and lets
    // quiet sessions collapse back to two rows.
    let mut context_items = Vec::new();
    let mut signal_items = Vec::new();
    if has_unseen_result {
        signal_items.push(
            div()
                .id(SharedString::from(format!(
                    "session-new-result-{}",
                    session.id
                )))
                .flex()
                .flex_none()
                .items_center()
                .px_1p5()
                .py(rems(0.125))
                .rounded_full()
                .bg(theme.muted.opacity(0.2))
                .text_xs()
                .font_medium()
                .text_color(theme.muted_foreground)
                .child("New result")
                .into_any_element(),
        );
    }
    if let (Some(snooze), Some(label)) = (session_snooze.as_ref(), snooze_label.as_ref()) {
        let snooze_tooltip = if snooze.pending() {
            format!("{label} — the session stays in its normal group until the save is confirmed")
        } else {
            format!("{label}\nReturns to its normal group when the deadline passes")
        };
        signal_items.push(
            div()
                .id(SharedString::from(format!(
                    "session-snoozed-{}",
                    session.id
                )))
                .debug_selector({
                    let id = session.id.clone();
                    move || format!("session-snoozed-{id}")
                })
                .flex()
                .flex_none()
                .min_w_0()
                .max_w_full()
                .items_center()
                .gap_1()
                .px_1p5()
                .py(rems(0.125))
                .rounded_full()
                .bg(theme.muted.opacity(0.35))
                .tooltip(move |window, cx| Tooltip::new(snooze_tooltip.clone()).build(window, cx))
                .child(
                    Icon::new(IconName::Moon)
                        .xsmall()
                        .text_color(theme.muted_foreground.opacity(0.9)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_medium()
                        .text_color(theme.muted_foreground.opacity(0.9))
                        .child(label.clone()),
                )
                .into_any_element(),
        );
    }
    if state_show_project {
        context_items.push(crate::session_project_label(project, cx).into_any_element());
    }

    if let Some(pr_chips) = pr_meta {
        signal_items.push(pr_chips.into_any_element());
    }

    context_items.extend(crate::session_branch_badge(session, cx));

    if let Some(git) = session_git_status {
        if !git.files.is_empty() {
            let changed_count = git.files.len();
            let additions: u32 = git.files.iter().map(|f| f.additions).sum();
            let deletions: u32 = git.files.iter().map(|f| f.deletions).sum();
            let git_tooltip = format!("{changed_count} changed files (+{additions} -{deletions})");
            signal_items.push(
                div()
                    .id(SharedString::from(format!(
                        "session-git-badge-{}",
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
                    .tooltip(move |window, cx| Tooltip::new(git_tooltip.clone()).build(window, cx))
                    .child(
                        div()
                            .size(rems(0.3125))
                            .rounded_full()
                            .bg(if additions > 0 {
                                theme.success
                            } else {
                                theme.warning
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_medium()
                            .text_color(theme.muted_foreground)
                            .child(format!("{changed_count}")),
                    )
                    .when(additions > 0 || deletions > 0, |this| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(rems(0.125))
                                .text_xs()
                                .when(additions > 0, |this| {
                                    this.child(
                                        div()
                                            .text_color(theme.success)
                                            .font_medium()
                                            .child(format!("+{additions}")),
                                    )
                                })
                                .when(deletions > 0, |this| {
                                    this.child(
                                        div()
                                            .text_color(theme.danger)
                                            .font_medium()
                                            .child(format!("-{deletions}")),
                                    )
                                }),
                        )
                    })
                    .into_any_element(),
            );
        }
    }

    if is_pinned {
        signal_items.push(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .px_1p5()
                .py(rems(0.125))
                .rounded_full()
                .bg(theme.primary.opacity(0.1))
                .text_xs()
                .font_medium()
                .text_color(theme.primary)
                .child(
                    Icon::default()
                        .path("icons/pin.svg")
                        .size(rems(0.625))
                        .text_color(theme.primary),
                )
                .child("Pinned")
                .into_any_element(),
        );
    }

    crate::session_card(&session.id, is_active, cx)
        .aria_label(session_row_label.clone())
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            select_row(crate::SidebarSessionAction::Open, window, cx)
        })
        .child(
            crate::session_card_content()
                .child(
                    crate::session_card_title_row()
                        .child(
                            Button::new(SharedString::from(format!(
                                "session-title-{}",
                                session.id
                            )))
                            .debug_selector({
                                let id = session.id.clone();
                                move || format!("session-title-{id}")
                            })
                            .accessibility_label(session_row_label.clone())
                            .tooltip(session_tooltip)
                            .ghost()
                            .xsmall()
                            .compact()
                            .flex_1()
                            .min_w_0()
                            .px_0()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(move |_, window, cx| {
                                select_title(crate::SidebarSessionAction::Open, window, cx)
                            })
                            .child(crate::session_title_text(session_title, is_active, cx)),
                        )
                        .child(
                            div()
                                .relative()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_1()
                                // Recency overlays the actions' stable intrinsic slot.
                                .child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_end()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .opacity(if is_active || is_pinned { 0.0 } else { 1.0 })
                                        .group_hover("session-card", |style| style.opacity(0.0))
                                        .child(time_ago),
                                )
                                .child(
                                    crate::session_pin_button(
                                        &session.id,
                                        is_pinned,
                                        is_active,
                                        cx,
                                    )
                                    .on_click(
                                        move |_, window, cx| {
                                            pin(crate::SidebarSessionAction::TogglePin, window, cx)
                                        },
                                    ),
                                )
                                .child(
                                    crate::session_archive_button(&session.id, is_active).on_click(
                                        move |_, window, cx| {
                                            archive(
                                                crate::SidebarSessionAction::Archive,
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                )
                                .child(
                                    crate::session_actions_button(&session.id, is_active)
                                        .dropdown_menu(quick_menu),
                                ),
                        ),
                )
                .child(crate::session_context_row().children(context_items))
                .when(
                    !signal_items.is_empty() || status_indicator.is_some(),
                    |el| {
                        el.child(
                            crate::session_signal_row()
                                .children(signal_items)
                                .children(status_indicator),
                        )
                    },
                ),
        )
        .context_menu(full_menu)
}

pub fn sidebar_pr_status_label(pr: &GitHubPrInfo) -> &'static str {
    if pr.state.eq_ignore_ascii_case("merged") {
        "Merged"
    } else if pr.is_draft || pr.state.eq_ignore_ascii_case("draft") {
        "Draft"
    } else if pr.state.eq_ignore_ascii_case("closed") {
        "Closed"
    } else {
        "Open"
    }
}

pub fn sidebar_pr_status_tooltip(pr: &GitHubPrInfo) -> String {
    format!(
        "PR #{} · {}\n{}\n{} → {}\nChecks: {} passed · {} pending · {} failed\nDiscussion: {} comments · {} review comments\n{}",
        pr.number,
        sidebar_pr_status_label(pr),
        pr.title,
        pr.head_ref,
        pr.base_ref,
        pr.passing_checks,
        pr.pending_checks,
        pr.failing_checks,
        pr.comments_count,
        pr.review_comments.len(),
        pr.url,
    )
}

pub fn session_time_ago(timestamp: u64, now: u64) -> String {
    let seconds = now.saturating_sub(timestamp);
    match seconds {
        0..=59 => "Just now".to_string(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}
