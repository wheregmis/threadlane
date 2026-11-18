//! Checkout review cards and menus. Hosts retain Git operations and confirmation guards.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use threadlane_protocol::repo::{GitFile, GitHubPrInfo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewFileAction {
    Stage(String),
    Unstage(String),
    Discard(ReviewDiscardTarget),
    IgnoreFile(String),
    IgnoreExtension(String),
    OpenDiff(String),
    CopyRelative(String),
    CopyAbsolute(String),
    Reveal(String),
}

pub fn review_file_manager_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Reveal in File Explorer"
    } else {
        "Reveal in File Manager"
    }
}

pub fn review_discard_menu(
    menu: PopupMenu,
    options: Vec<ReviewDiscardTarget>,
    busy: bool,
    on_discard: impl Fn(&ReviewDiscardTarget, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    // Match terminal menus: focus before drawing so the pinned context-menu
    // wrapper cannot publish a second focused accessibility node in prepaint.
    menu.focus_handle(cx).focus(window, cx);
    let request = std::rc::Rc::new(on_discard);
    options.into_iter().fold(menu, |menu, target| {
        let request = request.clone();
        menu.item(
            PopupMenuItem::new(target.label())
                .disabled(busy)
                .on_click(move |_, window, cx| request(&target, window, cx)),
        )
    })
}

pub struct ReviewFileMenu<'a> {
    pub file: &'a GitFile,
    pub selected_paths: &'a [String],
    pub total_files: usize,
    pub absolute_path: Option<&'a str>,
    pub reveal_label: &'a str,
    pub busy: bool,
}

/// Shared ordering and selection scope; callbacks never perform Git operations here.
pub fn review_file_menu(
    menu: PopupMenu,
    state: &ReviewFileMenu<'_>,
    on_action: impl Fn(&ReviewFileAction, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let request = std::rc::Rc::new(on_action);
    let item = |label: String, action: ReviewFileAction, disabled| {
        let request = request.clone();
        PopupMenuItem::new(label)
            .disabled(disabled)
            .on_click(move |_, window, cx| request(&action, window, cx))
    };
    let path = &state.file.path;
    let mut menu = menu.item(item(
        if state.file.staged {
            "Unstage File"
        } else {
            "Stage File"
        }
        .into(),
        if state.file.staged {
            ReviewFileAction::Unstage(path.clone())
        } else {
            ReviewFileAction::Stage(path.clone())
        },
        state.busy,
    ));
    let discard = request.clone();
    menu = review_discard_menu(
        menu,
        review_file_discard_targets(path, state.selected_paths, state.total_files),
        state.busy,
        move |target, window, cx| discard(&ReviewFileAction::Discard(target.clone()), window, cx),
        window,
        cx,
    );
    menu = menu.separator().item(item(
        "Ignore File (.gitignore)".into(),
        ReviewFileAction::IgnoreFile(path.clone()),
        state.busy,
    ));
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
    {
        menu = menu.item(item(
            format!("Ignore all *.{ext} files"),
            ReviewFileAction::IgnoreExtension(ext.into()),
            state.busy,
        ));
    }
    menu = menu
        .separator()
        .item(item(
            "Open Diff in Editor Tab".into(),
            ReviewFileAction::OpenDiff(path.clone()),
            false,
        ))
        .separator()
        .item(item(
            "Copy File Path".into(),
            ReviewFileAction::CopyRelative(path.clone()),
            false,
        ));
    if let Some(absolute) = state.absolute_path {
        menu = menu
            .item(item(
                "Copy Absolute File Path".into(),
                ReviewFileAction::CopyAbsolute(absolute.into()),
                false,
            ))
            .separator()
            .item(item(
                state.reveal_label.into(),
                ReviewFileAction::Reveal(absolute.into()),
                false,
            ));
    }
    menu
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewPrAction {
    Toggle,
    Open,
    FixCi,
    AddressComments,
}

pub struct ReviewPrState {
    pub expanded: bool,
    pub feedback_count: Option<usize>,
    pub can_address: bool,
    pub busy: bool,
}

/// Checkout-sized status and follow-ups, distinct from the full GitHub PR overview.
pub fn review_pr_card(
    pr: &GitHubPrInfo,
    state: &ReviewPrState,
    on_action: impl Fn(&ReviewPrAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let title = if pr.title.is_empty() {
        format!("PR #{}", pr.number)
    } else {
        format!("#{} {}", pr.number, pr.title)
    };
    let status = if pr.failing_checks > 0 {
        format!(
            "{} failing check{}",
            pr.failing_checks,
            if pr.failing_checks == 1 { "" } else { "s" }
        )
    } else if pr.pending_checks > 0 {
        format!("{} in progress", pr.pending_checks)
    } else if pr.total_checks == 0 {
        "No checks reported".into()
    } else {
        format!("All {} checks passed", pr.total_checks)
    };
    let color = if pr.failing_checks > 0 {
        theme.danger
    } else if pr.pending_checks > 0 {
        theme.warning
    } else {
        theme.muted_foreground
    };
    let icon = if pr.failing_checks > 0 {
        IconName::Close
    } else if pr.pending_checks > 0 {
        IconName::Asterisk
    } else if pr.total_checks > 0 {
        IconName::Check
    } else {
        IconName::Minus
    };
    let request = std::rc::Rc::new(on_action);
    let click = move |action| {
        let request = request.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| request(&action, window, cx)
    };
    div()
        .debug_selector(|| "review-pr-card".into())
        .flex_none()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1p5()
        .mx_3()
        .my_2()
        .p_2p5()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    Button::new("pr-card-toggle")
                        .debug_selector(|| "pr-card-toggle".into())
                        .accessibility_label(format!(
                            "{} pull request {title}; {status}",
                            if state.expanded { "Collapse" } else { "Expand" }
                        ))
                        .tooltip(if state.expanded { "Collapse" } else { "Expand" })
                        .ghost()
                        .h_auto()
                        .min_w_0()
                        .flex_1()
                        .p_0()
                        .on_click(click(ReviewPrAction::Toggle))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    Icon::new(if state.expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .xsmall()
                                    .flex_none(),
                                )
                                .child(
                                    Icon::default()
                                        .path("icons/git/actions.svg")
                                        .size_4()
                                        .flex_none(),
                                )
                                .child(
                                    div()
                                        .debug_selector(|| "review-pr-title".into())
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title.clone()),
                                ),
                        ),
                )
                .children((!pr.url.is_empty()).then(|| {
                    Button::new("pr-link-btn")
                        .debug_selector(|| "pr-link-btn".into())
                        .accessibility_label(format!("Open pull request {title} in browser"))
                        .tooltip("Open pull request in browser")
                        .icon(IconName::ExternalLink)
                        .ghost()
                        .xsmall()
                        .flex_none()
                        .on_click(click(ReviewPrAction::Open))
                })),
        )
        .when(state.expanded, |card| {
            card.child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .pt_0p5()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(Icon::new(icon).xsmall().flex_none().text_color(
                                if pr.total_checks > 0
                                    && pr.failing_checks == 0
                                    && pr.pending_checks == 0
                                {
                                    theme.success
                                } else {
                                    color
                                },
                            ))
                            .child(
                                div()
                                    .debug_selector(|| "review-pr-status".into())
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_xs()
                                    .text_color(color)
                                    .child(status),
                            ),
                    )
                    .child(if pr.failing_checks > 0 {
                        Button::new("fix-ci-btn")
                            .debug_selector(|| "fix-ci-btn".into())
                            .label("Fix CI")
                            .outline()
                            .xsmall()
                            .accessibility_label(format!(
                                "Ask AI to fix failing CI checks on PR #{}",
                                pr.number
                            ))
                            .tooltip("Ask AI to fix failing CI checks")
                            .disabled(state.busy)
                            .on_click(click(ReviewPrAction::FixCi))
                            .into_any_element()
                    } else if pr.total_checks == 0 {
                        div().into_any_element()
                    } else {
                        div()
                            .debug_selector(|| "review-pr-check-count".into())
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{}/{}", pr.passing_checks, pr.total_checks))
                            .into_any_element()
                    }),
            )
            .children(
                state
                    .feedback_count
                    .filter(|count| *count > 0)
                    .map(|count| {
                        div()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .pt_0p5()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(Icon::new(IconName::File).xsmall().flex_none())
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!(
                                                "{count} review comment{}",
                                                if count == 1 { "" } else { "s" }
                                            )),
                                    ),
                            )
                            .child(
                                Button::new("address-comments-btn")
                                    .debug_selector(|| "address-comments-btn".into())
                                    .label("Address")
                                    .ghost()
                                    .xsmall()
                                    .accessibility_label(format!(
                                        "Ask AI to address {count} review comments on PR #{}",
                                        pr.number
                                    ))
                                    .tooltip("Ask AI to address PR comments")
                                    .disabled(state.busy || !state.can_address)
                                    .on_click(click(ReviewPrAction::AddressComments)),
                            )
                    }),
            )
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewDiscardTarget {
    Single(String),
    Selected(Vec<String>),
    All(usize),
}

impl ReviewDiscardTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Single(_) => "Discard Changes...".to_string(),
            Self::Selected(paths) => format!("Discard Selected Changes ({})...", paths.len()),
            Self::All(count) => format!("Discard All Changes ({count})..."),
        }
    }

    pub fn requires_confirmation(&self) -> bool {
        matches!(self, Self::Selected(_) | Self::All(_))
    }

    pub fn confirmation_prompt(&self) -> Option<(String, String)> {
        match self {
            Self::Single(_) => None,
            Self::Selected(paths) => {
                let count = paths.len();
                let file_str = if count == 1 { "file" } else { "files" };
                Some((
                    "Discard selected changes?".to_string(),
                    format!(
                        "Are you sure you want to discard changes in {count} selected {file_str}? This cannot be undone."
                    ),
                ))
            }
            Self::All(count) => {
                let file_str = if *count == 1 { "file" } else { "files" };
                Some((
                    "Discard all changes?".to_string(),
                    format!(
                        "Are you sure you want to discard all changes across {count} {file_str}? This cannot be undone."
                    ),
                ))
            }
        }
    }
}

pub fn review_file_discard_targets(
    clicked_path: &str,
    selected_paths: &[String],
    total_files: usize,
) -> Vec<ReviewDiscardTarget> {
    let mut options = vec![ReviewDiscardTarget::Single(clicked_path.to_string())];
    let selected_count = selected_paths.len();
    if selected_count > 1 && selected_count < total_files {
        options.push(ReviewDiscardTarget::Selected(selected_paths.to_vec()));
    }
    if total_files > 1 {
        options.push(ReviewDiscardTarget::All(total_files));
    }
    options
}

pub fn review_selection_discard_targets(
    selected_paths: &[String],
    total_files: usize,
) -> Vec<ReviewDiscardTarget> {
    let mut options = Vec::new();
    let selected_count = selected_paths.len();
    if selected_count > 0 && selected_count < total_files {
        options.push(ReviewDiscardTarget::Selected(selected_paths.to_vec()));
    }
    if total_files > 0 {
        options.push(ReviewDiscardTarget::All(total_files));
    }
    options
}
