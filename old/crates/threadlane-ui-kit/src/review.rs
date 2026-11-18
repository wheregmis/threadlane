//! Controlled working-tree review components. Git services and mutation guards belong to hosts.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::{ScrollableElement, ScrollbarAxis};
use gpui_component::spinner::Spinner;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::TextViewState;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, InteractiveElementExt, Selectable, Sizable,
};
use threadlane_protocol::repo::{GitFile, GitStatus};

pub fn review_can_publish_branch(worktree_available: bool, status: Option<&GitStatus>) -> bool {
    worktree_available
        && status.is_some_and(|status| {
            !status.has_upstream
                && !status.detached
                && status.branch.is_some()
                && status.remote.is_some()
        })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewTab {
    #[default]
    Changes,
    History,
}

pub struct ReviewWorkspaceContext {
    pub repository: String,
    pub branch: String,
    pub git_state: String,
    pub worktree: bool,
    pub unavailable: bool,
    pub file: String,
    pub has_changes: bool,
}

pub fn review_workspace_context(
    state: &ReviewWorkspaceContext,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    div()
        .flex_none()
        .min_w_0()
        .px_3()
        .py_1p5()
        .bg(theme.list_head)
        .text_xs()
        .child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(state.repository.clone()),
                )
                .children(
                    state
                        .worktree
                        .then(|| Tag::secondary().child("worktree").xsmall()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(format!("· {}", state.branch)),
                )
                .when(state.has_changes, |row| {
                    row.child(
                        Button::new("open-review-from-context")
                            .label(state.git_state.clone())
                            .ghost()
                            .xsmall()
                            .accessibility_label(format!(
                                "Open Review, {}. Workspace changes.",
                                state.git_state
                            ))
                            .tooltip("Open Review (workspace changes)")
                            .on_click(on_open),
                    )
                })
                .children((!state.has_changes).then(|| {
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format!("· {}", state.git_state))
                })),
        )
        .child(
            div()
                .mt_0p5()
                .text_color(theme.muted_foreground)
                .truncate()
                .child(format!(
                    "{} · {}",
                    if state.unavailable {
                        "worktree unavailable"
                    } else {
                        "active worktree"
                    },
                    state.file
                )),
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewSyncKind {
    Publish,
    Pull,
    Push,
    Fetch,
}

pub fn review_sync_button(
    kind: ReviewSyncKind,
    count: usize,
    fetch_label: &str,
    busy: bool,
) -> Button {
    let (label, hint, icon) = match kind {
        ReviewSyncKind::Publish => (
            "Publish branch".into(),
            "Publish this branch to origin",
            Icon::new(IconName::ArrowUp),
        ),
        ReviewSyncKind::Pull => (
            format!("Pull ({count})"),
            "Pull latest changes from origin",
            Icon::new(IconName::ArrowDown),
        ),
        ReviewSyncKind::Push => (
            format!("Push ({count})"),
            "Push local commits to origin",
            Icon::new(IconName::ArrowUp),
        ),
        ReviewSyncKind::Fetch => (
            "Fetch".into(),
            fetch_label,
            Icon::default().path("icons/download.svg"),
        ),
    };
    Button::new("git-sync-action-btn")
        .debug_selector(|| "git-sync-action-btn".into())
        .icon(icon)
        .label(label)
        .accessibility_label(hint)
        .small()
        .tooltip(hint)
        .disabled(busy)
        .when(kind == ReviewSyncKind::Fetch, |button| button.ghost())
}

pub fn review_sync_actions(sync: Button, stash: Option<Button>, create_pr: Option<Button>) -> Div {
    div()
        .min_w_0()
        .max_w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .child(sync)
        .children(stash)
        .children(create_pr)
}

pub fn review_stash_button(busy: bool) -> Button {
    Button::new("git-stash-changes")
        .label("Stash…")
        .outline()
        .small()
        .accessibility_label("Stash changes")
        .tooltip("Stash changes…")
        .disabled(busy)
}

pub fn review_create_pr_button(busy: bool) -> Button {
    Button::new("git-create-pull-request").debug_selector(|| "git-create-pull-request".into())
        .icon(IconName::Github)
        .label("Create draft PR…")
        .accessibility_label("Review and create a draft pull request on GitHub")
        .outline()
        .small()
        .tooltip("Review and create a draft pull request on GitHub")
        .disabled(busy)
}

pub fn review_panel_surface() -> Div {
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .relative()
        .flex()
        .flex_col()
}

pub fn review_changes_body() -> Div {
    div().flex_1().min_w_0().min_h_0().flex().flex_col()
}

/// Details can overflow on short panels while commit controls remain visible.
pub fn review_changes_content() -> gpui_component::scroll::Scrollable<Div> {
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .overflow_y_scrollbar()
}

/// Keeps the virtual file list usable when surrounding details need to scroll.
pub fn review_changes_files(content: impl IntoElement) -> Div {
    div()
        .flex_1()
        .min_h(rems(8.0))
        .flex()
        .flex_col()
        .child(content)
}

pub fn review_branch_header(
    branch: &str,
    expanded: bool,
    sync_actions: impl IntoElement,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .gap_2()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.list_head)
        .child(
            Button::new("git-branch-selector-btn")
                .debug_selector(|| "git-branch-selector-btn".into())
                .accessibility_label(format!("Manage branches, current branch {branch}"))
                .ghost()
                .small()
                .selected(expanded)
                .flex()
                .items_center()
                .gap_2()
                .min_w(rems(5.0))
                // `flex_auto`, not `flex_1`: a zero basis never wraps, so the sync actions
                // kept their width and the branch name was cut to a few letters. Sized to its
                // name, the selector pushes the actions onto their own row when both don't fit.
                .flex_auto()
                .px_2()
                .py_1()
                .rounded_md()
                .on_click(on_toggle)
                .child(
                    div()
                        .size_4()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child(Icon::default().path("icons/git/branch.svg")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(branch.to_owned()),
                )
                .child(
                    div()
                        .size(rems(0.875))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child(if expanded {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        }),
                ),
        )
        .child(sync_actions)
}

pub fn review_tabs(
    active: ReviewTab,
    files: usize,
    staged: usize,
    commits: usize,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let changes = if staged > 0 {
        format!("Changes ({files}, {staged} staged)")
    } else if files > 0 {
        format!("Changes ({files})")
    } else {
        "Changes".into()
    };
    let history = if commits > 0 {
        format!("History ({commits})")
    } else {
        "History".into()
    };
    div()
        .flex_none()
        .border_b_1()
        .border_color(cx.theme().title_bar_border)
        .bg(cx.theme().title_bar)
        .px_3()
        .child(
            TabBar::new("review-sub-tabs")
                .segmented()
                .small()
                .selected_index(if active == ReviewTab::Changes { 0 } else { 1 })
                .children(vec![
                    Tab::new()
                        .debug_selector(|| "review-tab-changes".into())
                        .label(changes)
                        .aria_label(format!("Changes, {files} files, {staged} staged")),
                    Tab::new()
                        .debug_selector(|| "review-tab-history".into())
                        .label(history)
                        .aria_label(format!("History, {commits} recent commits")),
                ])
                .on_click(on_select),
        )
}

pub fn review_file_list(
    state: &ListState,
    render: impl FnMut(usize, &mut Window, &mut App) -> AnyElement + 'static,
) -> Div {
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .child(
            list(state.clone(), render)
                .size_full()
                .py_1()
                .with_sizing_behavior(ListSizingBehavior::Auto),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .child(gpui_component::scroll::Scrollbar::vertical(state)),
        )
}

pub fn review_folder_header(folder: &str, count: usize, collapsed: bool, cx: &App) -> Button {
    let label = if folder.is_empty() { "(root)" } else { folder };
    let hint = format!(
        "{} {label}, {count} changed files",
        if collapsed { "Expand" } else { "Collapse" }
    );
    Button::new(SharedString::from(format!("folder-btn-{folder}")))
        .debug_selector({
            let folder = folder.to_owned();
            move || format!("review-folder-{folder}")
        })
        .accessibility_label(hint.clone())
        .tooltip(hint)
        .ghost()
        .xsmall()
        .w_full()
        .justify_start()
        .px_2()
        .py_1()
        .child(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1p5()
                .child(
                    Icon::new(if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .size_3()
                    .flex_none()
                    .text_color(cx.theme().muted_foreground),
                )
                .child(
                    Icon::new(if collapsed {
                        IconName::Folder
                    } else {
                        IconName::FolderOpen
                    })
                    .size_3p5()
                    .flex_none()
                    .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(label.to_owned()),
                )
                .child(
                    Tag::new()
                        .child(count.to_string())
                        .with_variant(TagVariant::Secondary)
                        .small(),
                ),
        )
}

pub fn review_folder_files(cx: &App) -> Div {
    div()
        .min_w_0()
        .pl_3()
        .border_l_1()
        .border_color(cx.theme().border.opacity(0.4))
        .ml_3()
        .flex()
        .flex_col()
}

pub fn review_tree_viewport() -> gpui_component::scroll::Scrollable<Div> {
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_y_scrollbar()
        .py_1()
}

pub fn review_clean_state(refresh: Button, cx: &App) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .p_4()
        .text_center()
        .child(
            div()
                .mb_2()
                .size_8()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(cx.theme().success.opacity(0.12))
                .text_color(cx.theme().success)
                .child(Icon::new(IconName::Check)),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().foreground)
                .child("No changes"),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Working tree is clean"),
        )
        .child(div().mt_3().child(refresh))
}

pub fn review_refresh_button(busy: bool) -> Button {
    Button::new("refresh-clean-review")
        .debug_selector(|| "refresh-clean-review".into())
        .label("Refresh review")
        .ghost()
        .small()
        .accessibility_label("Refresh the working tree review")
        .tooltip("Refresh the working tree review")
        .disabled(busy)
}

pub fn review_no_results(
    on_clear: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    div()
        .debug_selector(|| "review-no-results".into())
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .p_4()
        .text_center()
        .child(div().text_sm().child("No matching changes"))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Try another file path or clear the filter."),
        )
        .child(
            Button::new("review-clear-no-results")
                .debug_selector(|| "review-clear-no-results".into())
                .label("Clear filter")
                .ghost()
                .small()
                .on_click(on_clear),
        )
}

pub fn review_diff_addition_percent(additions: u32, deletions: u32) -> f32 {
    match (additions, deletions) {
        (0, _) => 0.0,
        (_, 0) => 100.0,
        _ => (additions as f32 / (additions as f32 + deletions as f32) * 100.0).clamp(5.0, 95.0),
    }
}

pub fn review_diff_ratio(additions: u32, deletions: u32, cx: &App) -> Option<Div> {
    (additions > 0 || deletions > 0).then(|| {
        let add = review_diff_addition_percent(additions, deletions) / 100.0;
        div().px_3().py_0p5().child(
            div()
                .w_full()
                .h(rems(0.25))
                .rounded_full()
                .overflow_hidden()
                .bg(cx.theme().muted)
                .flex()
                .child(div().h_full().w(relative(add)).bg(cx.theme().success))
                .child(div().h_full().w(relative(1.0 - add)).bg(cx.theme().danger)),
        )
    })
}

pub enum ReviewDiffContent<'a> {
    Loading,
    Failed(&'a str),
    Empty,
    Ready(&'a Entity<TextViewState>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDiffAction {
    Retry,
    ShowWhitespace,
}

pub fn review_diff_body(
    content: ReviewDiffContent<'_>,
    ignore_whitespace: bool,
    scroll_id: ElementId,
    on_action: impl Fn(&ReviewDiffAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    review_diff_body_scrolled(
        content,
        ignore_whitespace,
        scroll_id,
        None,
        std::rc::Rc::new(std::cell::Cell::new(false)),
        on_action,
        cx,
    )
}

pub(crate) fn review_diff_body_scrolled(
    content: ReviewDiffContent<'_>,
    ignore_whitespace: bool,
    scroll_id: ElementId,
    scroll: Option<&ScrollHandle>,
    reveal_enabled: std::rc::Rc<std::cell::Cell<bool>>,
    on_action: impl Fn(&ReviewDiffAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let callback = std::rc::Rc::new(on_action);
    let request = move |action| {
        let callback = callback.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    let content = match content {
        ReviewDiffContent::Loading => div()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Spinner::new().small())
                    .child("Updating diff…"),
            )
            .into_any_element(),
        ReviewDiffContent::Failed(error) => div()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .child("Could not load diff")
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(error.to_owned()),
                    )
                    .child(
                        Button::new("retry-review-diff")
                            .debug_selector(|| "retry-review-diff".into())
                            .small()
                            .label("Retry")
                            .on_click(request(ReviewDiffAction::Retry)),
                    ),
            )
            .into_any_element(),
        ReviewDiffContent::Empty => div()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .child(if ignore_whitespace {
                        "No text changes to show with whitespace ignored"
                    } else {
                        "No text changes to show"
                    })
                    .children(ignore_whitespace.then(|| {
                        Button::new("show-whitespace-changes")
                            .debug_selector(|| "show-whitespace-changes".into())
                            .small()
                            .label("Show whitespace changes")
                            .on_click(request(ReviewDiffAction::ShowWhitespace))
                    })),
            )
            .into_any_element(),
        ReviewDiffContent::Ready(text) => crate::diff_text_view(text, cx)
            .when_some(scroll, |view, scroll| {
                let scroll = scroll.clone();
                view.on_reveal(move |line, _, _| {
                    if !reveal_enabled.get() {
                        return;
                    }
                    let viewport = scroll.bounds();
                    let mut offset = scroll.offset();
                    if line.bottom() > viewport.bottom() {
                        offset.y -= line.bottom() - viewport.bottom();
                    } else if line.top() < viewport.top() {
                        offset.y += viewport.top() - line.top();
                    }
                    scroll.set_offset(offset);
                })
            })
            .into_any_element(),
    };
    if let Some(scroll) = scroll {
        div()
            .id(scroll_id)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .track_scroll(scroll)
            .overflow_y_scroll()
            .lock_scroll_axis()
            .p_3()
            .scrollbar(scroll, ScrollbarAxis::Vertical)
            .child(content)
            .into_any_element()
    } else {
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scrollbar()
            .id(scroll_id)
            .p_3()
            .child(content)
            .into_any_element()
    }
}

/// Ordered position of a file inside the filtered working-tree inventory.
/// Membership and position resolve by exact path, never by a stored row index,
/// so a background refresh cannot retarget navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewDiffAdjacency {
    /// 1-based position of the current file.
    pub position: usize,
    pub total: usize,
    pub previous: Option<String>,
    pub next: Option<String>,
}

/// Bounded previous/next resolution over the filtered inventory's flat order.
/// No wrapping: the first file has no previous, the last has no next.
pub fn review_diff_adjacency(paths: &[String], current: &str) -> Option<ReviewDiffAdjacency> {
    let index = paths.iter().position(|path| path == current)?;
    Some(ReviewDiffAdjacency {
        position: index + 1,
        total: paths.len(),
        previous: index.checked_sub(1).map(|prev| paths[prev].clone()),
        next: paths.get(index + 1).cloned(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDiffNavAction {
    Previous,
    Next,
}

/// Inputs for the single-file Review diff navigation row. The host owns the
/// inventory, the diff loader and all Git state; this row only requests a
/// direction and never stages, commits, discards, or marks files.
pub struct ReviewDiffNavigation<'a> {
    /// Ordered full relative paths of the filtered inventory.
    pub paths: &'a [String],
    /// Currently requested file. `None`/absent shows the removed-file state.
    pub current: Option<&'a str>,
    /// Active filter query when the inventory is filtered.
    pub filter: Option<&'a str>,
    /// Textual reason navigation is unavailable (inventory unavailable,
    /// checkout switching).
    pub unavailable: Option<&'a str>,
    /// Stable focus target for when the initiating control becomes disabled
    /// at a boundary.
    pub focus: Option<&'a FocusHandle>,
}

/// `Previous file · File X of Y · Next file` for a single-file local Review
/// diff. Hosts derive `paths` from their filtered inventory each render.
pub fn review_diff_nav(
    nav: &ReviewDiffNavigation,
    on_navigate: impl Fn(&ReviewDiffNavAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme().colors;
    let adjacency = nav
        .current
        .and_then(|current| review_diff_adjacency(nav.paths, current));
    let blocked = nav.unavailable.is_some();
    let previous = adjacency.as_ref().and_then(|adj| adj.previous.clone());
    let next = adjacency.as_ref().and_then(|adj| adj.next.clone());
    let status = if let Some(reason) = nav.unavailable {
        reason.to_owned()
    } else if let Some(adjacency) = &adjacency {
        match nav.filter {
            Some(query) => format!(
                "File {} of {} matching files · filter \"{query}\"",
                adjacency.position, adjacency.total
            ),
            None => format!("File {} of {}", adjacency.position, adjacency.total),
        }
    } else {
        "File no longer in the current changes list".to_owned()
    };
    let previous_hint = previous.clone().map_or_else(
        || "At the first file".to_owned(),
        |path| format!("Previous file · {path}"),
    );
    let next_hint = next.clone().map_or_else(
        || "At the last file".to_owned(),
        |path| format!("Next file · {path}"),
    );
    let filter_help = nav
        .filter
        .map(|query| format!(" filtered by \"{query}\""))
        .unwrap_or_default();
    let callback = std::rc::Rc::new(on_navigate);
    let request = move |action: ReviewDiffNavAction| {
        let callback = callback.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    div()
        .id("review-diff-nav")
        .debug_selector(|| "review-diff-nav".into())
        .role(Role::Group)
        .aria_label(format!("Changed file navigation{filter_help}"))
        .when_some(nav.focus, |row, focus| row.track_focus(focus))
        .flex_none()
        .min_w_0()
        .px_3()
        .py_1()
        .flex()
        .items_center()
        .gap_1()
        .child(
            Button::new("review-diff-prev")
                .debug_selector(|| "review-diff-prev".into())
                .icon(IconName::ChevronLeft)
                .label("Previous file")
                .accessibility_label(previous_hint.clone())
                .tooltip(previous_hint)
                .ghost()
                .xsmall()
                .disabled(blocked || previous.is_none())
                .on_click(request(ReviewDiffNavAction::Previous)),
        )
        .child(
            div()
                .debug_selector(|| "review-diff-nav-position".into())
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(status),
        )
        .child(
            Button::new("review-diff-next")
                .debug_selector(|| "review-diff-next".into())
                .icon(IconName::ChevronRight)
                .label("Next file")
                .accessibility_label(next_hint.clone())
                .tooltip(next_hint)
                .ghost()
                .xsmall()
                .disabled(blocked || next.is_none())
                .on_click(request(ReviewDiffNavAction::Next)),
        )
        .into_any_element()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewViewMode {
    #[default]
    List,
    Tree,
}

pub struct ReviewFileAppearance<'a> {
    pub selected: bool,
    pub open: bool,
    pub tree_node: bool,
    pub absolute_path: Option<&'a str>,
}

/// Working-tree selection is independent of the open diff. PR rows use viewed markers
/// and a multiline layout, so this compact row keeps the two controls separate.
pub fn review_file_row(
    file: &GitFile,
    appearance: ReviewFileAppearance<'_>,
    on_select: impl Fn(&bool, &mut Window, &mut App) + 'static,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let path = file.path.clone();
    let (directory, filename) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
    let directory = directory.to_owned();
    let filename = filename.to_owned();
    let status = file.status_char().to_string();
    let (status_color, status_bg) = match file.status_char() {
        'A' | '?' => (theme.success, theme.success.opacity(0.15)),
        'D' => (theme.danger, theme.danger.opacity(0.15)),
        'R' => (theme.link, theme.link.opacity(0.15)),
        _ => (theme.warning, theme.warning.opacity(0.15)),
    };

    let row_id = SharedString::from(format!("review-file-{path}"));
    div()
        .id(row_id)
        .debug_selector(|| "review-file-row".into())
        .role(Role::Group)
        .aria_label(format!("Changed file {path}"))
        .w_full()
        .min_w_0()
        .h_8()
        .min_h_8()
        .max_h_8()
        .flex_shrink_0()
        .overflow_hidden()
        .px_2()
        .rounded_md()
        .flex()
        .items_center()
        .gap_2()
        .bg(if appearance.selected {
            theme.list_active
        } else {
            gpui::transparent_black()
        })
        .hover(|row| {
            row.bg(if appearance.selected {
                theme.list_active_border.opacity(0.35)
            } else {
                theme.list_hover
            })
        })
        .focus(|row| row.border_color(theme.ring))
        .child(
            Checkbox::new(SharedString::from(format!("chk-{path}")))
                .debug_selector({
                    let path = path.clone();
                    move || format!("review-select-{path}")
                })
                .accessibility_label(format!("Select {path} for Git actions"))
                .checked(appearance.selected)
                .small()
                .on_click(on_select),
        )
        .child(
            Button::new(SharedString::from(format!("review-file-btn-{path}")))
                .debug_selector({
                    let path = path.clone();
                    move || format!("review-open-{path}")
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .debug_selector(|| "review-filename".into())
                                .min_w(px(40.0))
                                .max_w_full()
                                .truncate()
                                .child(filename),
                        )
                        .when(!directory.is_empty() && !appearance.tree_node, |row| {
                            row.child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_ellipsis_start()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(directory),
                            )
                        }),
                )
                .accessibility_label(format!(
                    "Review {path}, status {status}, {} additions, {} deletions",
                    file.additions, file.deletions
                ))
                .tooltip(format!(
                    "Review {path} · {status} · +{} −{}{}",
                    file.additions,
                    file.deletions,
                    appearance
                        .absolute_path
                        .map(|abs| format!("\n{abs}"))
                        .unwrap_or_default()
                ))
                .ghost()
                .small()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .justify_start()
                .selected(appearance.open)
                .on_click(on_open),
        )
        .child(
            div()
                .debug_selector(|| "review-file-status".into())
                .flex_none()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .bg(status_bg)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(status_color)
                .child(status),
        )
        .children((file.additions > 0 || file.deletions > 0).then(|| {
            div()
                .debug_selector(|| "review-file-stats".into())
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .children((file.additions > 0).then(|| {
                    div()
                        .text_color(theme.success)
                        .child(format!("+{}", file.additions))
                }))
                .children((file.deletions > 0).then(|| {
                    div()
                        .text_color(theme.danger)
                        .child(format!("\u{2212}{}", file.deletions))
                }))
        }))
}

pub fn review_file_inset(row: impl IntoElement) -> Div {
    div().w_full().min_w_0().px_2().child(row)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    ClearFilter,
    SelectList,
    SelectTree,
    SelectAll(bool),
    OpenCombinedDiff,
    StageAll,
    UnstageAll,
    ClearCommit,
    GenerateCommit,
    Commit,
    CommitAndPush,
    Push,
}

pub fn review_discard_button(busy: bool) -> Button {
    Button::new("selection-bar-discard-btn")
        .icon(IconName::Undo2)
        .accessibility_label("Discard changes; right-click for more options")
        .tooltip("Discard changes (right-click for more options)")
        .ghost()
        .xsmall()
        .disabled(busy)
}

pub fn review_toolbar_surface(cx: &App) -> Div {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .min_w_0()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().title_bar)
}

pub fn review_filters(
    filter: &Entity<InputState>,
    view_mode: ReviewViewMode,
    discard: AnyElement,
    on_action: impl Fn(&ReviewAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let callback = std::rc::Rc::new(on_action);
    let request = move |action| {
        let callback = callback.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme.input)
                .border_1()
                .border_color(theme.border)
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .size(rems(0.875))
                        .text_color(theme.muted_foreground)
                        .child(IconName::Search),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(filter)
                            .aria_label("Filter changes")
                            .appearance(false)
                            .bordered(false),
                    ),
                )
                .children((!filter.read(cx).value().is_empty()).then(|| {
                    Button::new("clear-review-filter-btn")
                        .debug_selector(|| "clear-review-filter-btn".into())
                        .icon(IconName::Close)
                        .accessibility_label("Clear filter")
                        .ghost()
                        .xsmall()
                        .tooltip("Clear filter")
                        .on_click(request(ReviewAction::ClearFilter))
                })),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .rounded_md()
                .bg(theme.tab_bar_segmented)
                .p_0p5()
                .child(
                    Button::new("review-view-list")
                        .debug_selector(|| "review-view-list".into())
                        .icon(IconName::Menu)
                        .accessibility_label("Flat list view")
                        .ghost()
                        .xsmall()
                        .selected(view_mode == ReviewViewMode::List)
                        .tooltip("Flat list view")
                        .on_click(request(ReviewAction::SelectList)),
                )
                .child(
                    Button::new("review-view-tree")
                        .debug_selector(|| "review-view-tree".into())
                        .icon(IconName::FolderOpen)
                        .accessibility_label("Tree view")
                        .ghost()
                        .xsmall()
                        .selected(view_mode == ReviewViewMode::Tree)
                        .tooltip("Tree view")
                        .on_click(request(ReviewAction::SelectTree)),
                ),
        )
        .child(discard)
}

pub struct ReviewSelectionState {
    pub selected_count: usize,
    pub total_files: usize,
    pub additions: u32,
    pub deletions: u32,
    pub unstaged_count: usize,
    pub has_staged: bool,
    pub busy: bool,
}

pub fn review_selection_bar(
    state: &ReviewSelectionState,
    on_action: impl Fn(&ReviewAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let all_selected = state.total_files > 0 && state.selected_count == state.total_files;
    let callback = std::rc::Rc::new(on_action);
    let clicks = callback.clone();
    let request = move |action| {
        let callback = clicks.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    div()
        .min_w_0()
        .flex()
        .flex_wrap()
        .gap_2()
        .items_center()
        .justify_between()
        .px_3()
        .py_1()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.list_head)
        .text_xs()
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    Checkbox::new("select-all-files")
                        .debug_selector(|| "select-all-files".into())
                        .accessibility_label(format!(
                            "Select all {} changed files",
                            state.total_files
                        ))
                        .checked(all_selected)
                        .small()
                        .disabled(state.busy)
                        .on_click(move |checked, window, cx| {
                            callback(&ReviewAction::SelectAll(*checked), window, cx)
                        }),
                )
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(format!(
                            "{}/{} files",
                            state.selected_count, state.total_files
                        )),
                )
                .when(state.additions > 0 || state.deletions > 0, |stats| {
                    stats
                        .child(
                            div()
                                .text_color(theme.success)
                                .child(format!("+{}", state.additions)),
                        )
                        .child(
                            div()
                                .text_color(theme.danger)
                                .child(format!("\u{2212}{}", state.deletions)),
                        )
                })
                .child(
                    Button::new("view-combined-diff-btn")
                        .debug_selector(|| "view-combined-diff-btn".into())
                        .label("View Diff")
                        .accessibility_label("Open combined diff of all changes")
                        .ghost()
                        .xsmall()
                        .tooltip("Open combined diff of all changes")
                        .on_click(request(ReviewAction::OpenCombinedDiff)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new("git-stage-all-btn")
                        .debug_selector(|| "git-stage-all-btn".into())
                        .label("Stage all")
                        .accessibility_label("Stage all changes (git add -A)")
                        .ghost()
                        .xsmall()
                        .disabled(state.busy || state.unstaged_count == 0)
                        .tooltip("Stage all changes (git add -A)")
                        .on_click(request(ReviewAction::StageAll)),
                )
                .when(state.has_staged, |row| {
                    row.child(
                        Button::new("git-unstage-all-btn")
                            .debug_selector(|| "git-unstage-all-btn".into())
                            .label("Unstage all")
                            .accessibility_label("Unstage all changes (git restore --staged .)")
                            .ghost()
                            .xsmall()
                            .disabled(state.busy)
                            .tooltip("Unstage all changes (git restore --staged .)")
                            .on_click(request(ReviewAction::UnstageAll)),
                    )
                }),
        )
}

pub struct ReviewCommitState {
    pub selected_count: usize,
    pub total_files: usize,
    pub busy: bool,
    pub generating: bool,
    pub can_push: bool,
}

pub fn review_commit_footer(
    input: &Entity<InputState>,
    state: &ReviewCommitState,
    on_action: impl Fn(&ReviewAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let callback = std::rc::Rc::new(on_action);
    let request = move |action| {
        let callback = callback.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    let commit_label = if state.selected_count > 0 && state.selected_count < state.total_files {
        format!("Commit {}", state.selected_count)
    } else {
        "Commit".to_string()
    };
    let commit_push_label = if state.selected_count > 0 && state.selected_count < state.total_files
    {
        format!("Commit {} & push", state.selected_count)
    } else {
        "Commit & push".to_string()
    };

    let commit_val = input.read(cx).value();
    let first_line = commit_val.lines().next().unwrap_or("");
    let subject_len = first_line.chars().count();
    let counter_color = if subject_len > 72 {
        theme.danger
    } else if subject_len > 50 {
        theme.warning
    } else {
        theme.muted_foreground
    };

    let is_empty = commit_val.trim().is_empty();
    let can_commit = !is_empty && state.selected_count > 0 && !state.busy;

    div()
        .debug_selector(|| "review-commit-footer".into())
        .min_w_0()
        .flex_none()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_3()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
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
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.muted_foreground)
                                .child("COMMIT"),
                        )
                        .when(subject_len > 0, |header| {
                            header.child(
                                div()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded_sm()
                                    .bg(counter_color.opacity(0.12))
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(counter_color)
                                    .child(if subject_len > 72 {
                                        format!("{subject_len}/72 (too long)")
                                    } else if subject_len > 50 {
                                        format!("{subject_len}/50")
                                    } else {
                                        format!("{subject_len}")
                                    }),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children((!commit_val.is_empty()).then(|| {
                            Button::new("clear-commit-input")
                                .debug_selector(|| "clear-commit-input".into())
                                .icon(IconName::Close)
                                .accessibility_label("Clear message")
                                .ghost()
                                .xsmall()
                                .tooltip("Clear message")
                                .on_click(request(ReviewAction::ClearCommit))
                        }))
                        .child(if state.generating {
                            Button::new("git-generate-commit-msg")
                                .debug_selector(|| "git-generate-commit-msg".into())
                                .child(Spinner::new().xsmall())
                                .accessibility_label("Generating commit message with AI…")
                                .ghost()
                                .xsmall()
                                .disabled(true)
                                .tooltip("Generating commit message with AI…")
                        } else {
                            Button::new("git-generate-commit-msg")
                                .debug_selector(|| "git-generate-commit-msg".into())
                                .icon(IconName::Bot)
                                .accessibility_label("Generate commit message with AI")
                                .ghost()
                                .xsmall()
                                .tooltip("Generate commit message with AI")
                                .disabled(state.busy || state.total_files == 0)
                                .on_click(request(ReviewAction::GenerateCommit))
                        }),
                ),
        )
        .child(
            div()
                .px_2p5()
                .py_2()
                .rounded_md()
                .bg(theme.input)
                .border_1()
                .border_color(theme.border)
                .focus(|d| d.border_color(theme.ring))
                .child(
                    Input::new(input)
                        .aria_label("Commit summary")
                        .disabled(state.busy),
                ),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    Button::new("git-commit-and-push")
                        .debug_selector(|| "git-commit-and-push".into())
                        .icon(Icon::default().path("icons/git/commit.svg"))
                        .label(commit_push_label)
                        .min_w(rems(9.0))
                        .primary()
                        .small()
                        .flex_1()
                        .tooltip(if can_commit {
                            "Commit the selected changes and push"
                        } else {
                            "Select files and write a message to commit"
                        })
                        .disabled(!can_commit)
                        .on_click(request(ReviewAction::CommitAndPush)),
                )
                .child(
                    Button::new("git-commit-only")
                        .debug_selector(|| "git-commit-only".into())
                        .label(commit_label)
                        .outline()
                        .small()
                        .tooltip(if can_commit {
                            "Commit the selected changes locally"
                        } else {
                            "Select files and write a message to commit"
                        })
                        .disabled(!can_commit)
                        .on_click(request(ReviewAction::Commit)),
                )
                .when(state.can_push, |row| {
                    row.child(
                        Button::new("git-push-only")
                            .debug_selector(|| "git-push-only".into())
                            .accessibility_label("Push commits")
                            .icon(Icon::default().path("icons/git/actions.svg"))
                            .tooltip("Push commits")
                            .ghost()
                            .small()
                            .on_click(request(ReviewAction::Push)),
                    )
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::review_diff_adjacency;

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn adjacency_is_bounded_and_exact() {
        let files = paths(&["a.rs", "dir/b.rs", "b.rs", "c.rs"]);
        let first = review_diff_adjacency(&files, "a.rs").unwrap();
        assert_eq!(first.position, 1);
        assert_eq!(first.total, 4);
        assert_eq!(first.previous, None);
        assert_eq!(first.next.as_deref(), Some("dir/b.rs"));
        // Exact paths distinguish duplicate basenames.
        let nested = review_diff_adjacency(&files, "dir/b.rs").unwrap();
        assert_eq!(nested.position, 2);
        assert_eq!(nested.next.as_deref(), Some("b.rs"));
        let last = review_diff_adjacency(&files, "c.rs").unwrap();
        assert_eq!(last.position, 4);
        assert_eq!(last.next, None);
        assert!(review_diff_adjacency(&files, "b.rs").is_some());
        assert!(review_diff_adjacency(&files, "dir\\b.rs").is_none());
        assert!(review_diff_adjacency(&files, " missing.rs").is_none());
    }

    #[test]
    fn adjacency_handles_special_and_single_entries() {
        let files = paths(&["All changes", "sp ace.rs", "ünïcode/文件.rs"]);
        let all_changes = review_diff_adjacency(&files, "All changes").unwrap();
        assert_eq!(all_changes.position, 1);
        let unicode = review_diff_adjacency(&files, "ünïcode/文件.rs").unwrap();
        assert_eq!(unicode.position, 3);
        assert_eq!(unicode.next, None);
        let single = paths(&["only.rs"]);
        let only = review_diff_adjacency(&single, "only.rs").unwrap();
        assert_eq!((only.position, only.total), (1, 1));
        assert_eq!((only.previous, only.next), (None, None));
        assert!(review_diff_adjacency(&[], "only.rs").is_none());
    }
}
