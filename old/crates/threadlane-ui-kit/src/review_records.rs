//! Recorded changes. Hosts load files/diffs and retain expansion state.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::{Scrollable, ScrollableElement};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Disableable, IconName, Sizable};
use threadlane_protocol::repo::{GitCommitInfo, GitFile, GitStashInfo};

pub fn review_filtered_commits<'a>(
    commits: &'a [GitCommitInfo],
    query: &str,
) -> Vec<&'a GitCommitInfo> {
    let query = query.trim().to_lowercase();
    commits
        .iter()
        .filter(|commit| {
            query.is_empty()
                || [
                    &commit.summary,
                    &commit.author_name,
                    &commit.short_sha,
                    &commit.sha,
                ]
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
        })
        .collect()
}

pub fn review_history_surface(input: &Entity<InputState>, content: AnyElement, cx: &App) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().title_bar)
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().input)
                        .child(
                            div()
                                .size(rems(0.875))
                                .flex_none()
                                .text_color(cx.theme().muted_foreground)
                                .child(IconName::Search),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(input)
                                    .aria_label("Filter commits")
                                    .appearance(false)
                                    .bordered(false),
                            ),
                        ),
                ),
        )
        .child(content)
}

pub fn review_history_empty(
    filtered: bool,
    on_clear: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    div()
        .debug_selector(|| "review-history-empty".into())
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
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().foreground)
                .child("No commits found"),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if filtered {
                    "No commits match your filter."
                } else {
                    "This branch has no recent commits."
                }),
        )
        .children(filtered.then(|| {
            Button::new("history-clear-filter")
                .debug_selector(|| "history-clear-filter".into())
                .label("Clear filter")
                .ghost()
                .small()
                .accessibility_label("Clear the commit filter")
                .tooltip("Clear the commit filter")
                .on_click(on_clear)
        }))
}

pub fn review_history_viewport() -> Scrollable<Div> {
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_y_scrollbar()
        .py_1()
}

/// These expandable checkout cards differ from the PR discussion's static
/// timeline rows: selection owns asynchronous changed-file details.
pub fn review_commit_card(
    commit: &GitCommitInfo,
    expanded: bool,
    files: Option<AnyElement>,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let sha = commit.sha.clone();
    div()
        .id(SharedString::from(format!("commit-{sha}")))
        .min_w_0()
        .flex()
        .flex_col()
        .mx_3()
        .my_0p5()
        .rounded_lg()
        .border_1()
        .border_color(if expanded {
            theme.primary.opacity(0.6)
        } else {
            theme.border
        })
        .bg(theme.group_box)
        .child(
            Button::new(SharedString::from(format!("commit-header-{sha}")))
                .debug_selector(move || format!("commit-header-{sha}"))
                .accessibility_label(format!(
                    "{} commit {}: {}, {}, {}",
                    if expanded { "Collapse" } else { "Inspect" },
                    commit.short_sha,
                    commit.summary,
                    commit.author_name,
                    commit.relative_time
                ))
                .ghost()
                .h_auto()
                .w_full()
                .min_w_0()
                .p_0()
                .on_click(on_toggle)
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .p_2p5()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .debug_selector(|| "review-commit-summary".into())
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .child(commit.summary.clone()),
                                )
                                .child(
                                    div()
                                        .debug_selector(|| "review-commit-sha".into())
                                        .flex_none()
                                        .px_1p5()
                                        .py_0p5()
                                        .rounded_sm()
                                        .bg(theme.muted)
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(commit.short_sha.clone()),
                                ),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(
                                    div()
                                        .size_3()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(IconName::User),
                                )
                                .child(div().min_w_0().truncate().child(format!(
                                    "{} • {}",
                                    commit.author_name, commit.relative_time
                                ))),
                        ),
                ),
        )
        .children(files)
}

pub fn review_commit_files(loading: bool, files: Vec<AnyElement>, cx: &App) -> Div {
    div()
        .debug_selector(|| "review-commit-files".into())
        .min_w_0()
        .border_t_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .p_2()
        .flex()
        .flex_col()
        .gap_1()
        .children(loading.then(|| {
            div()
                .p_2()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(Spinner::new().xsmall())
                .child("Loading changed files…")
        }))
        .children((!loading && files.is_empty()).then(|| {
            div()
                .p_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("No files changed in this commit.")
        }))
        .children(files)
}

pub fn review_commit_file(
    sha: &str,
    file: &GitFile,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Button {
    let theme = cx.theme().colors;
    let path = file.path.clone();
    let id = format!("commit-file-{sha}-{path}");
    let status = file.status_char();
    let color = match status {
        'A' | '?' => theme.success,
        'D' => theme.danger,
        _ => theme.warning,
    };
    Button::new(SharedString::from(id.clone()))
        .debug_selector(move || id.clone())
        .accessibility_label(format!(
            "Review {path} in commit {sha}, status {status}, {} additions, {} deletions",
            file.additions, file.deletions
        ))
        .ghost()
        .h_auto()
        .w_full()
        .min_w_0()
        .p_0()
        .on_click(on_open)
        .child(
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .h(rems(STASH_FILE_HEIGHT))
                .px_2()
                .rounded_md()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .size_3()
                                .flex_none()
                                .text_color(theme.muted_foreground)
                                .child(IconName::File),
                        )
                        .child(
                            div()
                                .debug_selector(|| "review-recorded-file-path".into())
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme.foreground)
                                .child(path),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "review-recorded-file-stats".into())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .children((file.additions > 0).then(|| {
                            div()
                                .text_xs()
                                .text_color(theme.success)
                                .child(format!("+{}", file.additions))
                        }))
                        .children((file.deletions > 0).then(|| {
                            div()
                                .text_xs()
                                .text_color(theme.danger)
                                .child(format!("-{}", file.deletions))
                        }))
                        .child(
                            div()
                                .size(rems(0.875))
                                .rounded_sm()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(color)
                                .child(status.to_string()),
                        ),
                ),
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewStashAction {
    Toggle,
    Discard,
    Restore,
}

pub struct ReviewStashState {
    pub expanded: bool,
    pub loading: bool,
    pub files_count: Option<usize>,
    pub busy: bool,
}

pub fn review_stash_card(
    stash: &GitStashInfo,
    state: &ReviewStashState,
    files: Option<AnyElement>,
    on_action: impl Fn(&ReviewStashAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let count = if state.loading {
        "Loading files…".into()
    } else {
        match state.files_count {
            Some(1) => "1 file".into(),
            Some(count) => format!("{count} files"),
            None => "Stashed changes".into(),
        }
    };
    let time = if stash.relative_time.is_empty() {
        String::new()
    } else {
        format!(" • {}", stash.relative_time)
    };
    let callback = std::rc::Rc::new(on_action);
    let request = move |action| {
        let callback = callback.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    div()
        .id("stash-banner")
        .debug_selector(|| "stash-banner".into())
        .flex_none()
        .min_w_0()
        .min_h_0()
        .mx_3()
        .my_2()
        .p_2p5()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            Button::new("stash-header-toggle")
                .debug_selector(|| "stash-header-toggle".into())
                .accessibility_label(format!(
                    "{} stashed changes, {count}{time}: {}",
                    if state.expanded { "Hide" } else { "Show" },
                    stash.message
                ))
                .tooltip(if state.expanded { "Collapse" } else { "Expand" })
                .ghost()
                .flex_none()
                .h_auto()
                .w_full()
                .min_w_0()
                .p_0()
                .on_click(request(ReviewStashAction::Toggle))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .size(rems(0.875))
                                .flex_none()
                                .text_color(theme.primary)
                                .child(if state.expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                }),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(theme.foreground)
                                        .child("Stashed changes"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("({count}{time})")),
                                ),
                        ),
                ),
        )
        .child(
            div()
                .flex_none()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(if stash.message.is_empty() {
                    "Stashed changes on this branch".into()
                } else {
                    stash.message.clone()
                }),
        )
        .children(files)
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_2()
                .pt_1()
                .child(
                    Button::new("discard-stash-btn")
                        .debug_selector(|| "discard-stash-btn".into())
                        .label("Discard")
                        .danger()
                        .xsmall()
                        .accessibility_label("Discard the stashed changes")
                        .tooltip("Discard the stashed changes")
                        .disabled(state.busy)
                        .on_click(request(ReviewStashAction::Discard)),
                )
                .child(
                    Button::new("restore-stash-btn")
                        .debug_selector(|| "restore-stash-btn".into())
                        .label("Restore stash")
                        .outline()
                        .xsmall()
                        .accessibility_label("Restore the stashed changes")
                        .tooltip("Restore the stashed changes")
                        .disabled(state.busy)
                        .on_click(request(ReviewStashAction::Restore)),
                ),
        )
}

// Match the fixed row metric when sizing the bounded stash viewport.
const STASH_FILE_HEIGHT: f32 = 1.625;

pub fn review_stash_files(loading: bool, files: Vec<AnyElement>, cx: &App) -> Scrollable<Div> {
    let rows = files.len().max(usize::from(loading));
    div()
        .h(rems(0.875 + rows as f32 * (STASH_FILE_HEIGHT + 0.25)))
        .min_w_0()
        .min_h_0()
        .max_h(rems(14.0))
        .flex_shrink_1()
        .flex()
        .flex_col()
        .gap_1()
        .my_1()
        .p_1p5()
        .rounded_md()
        .bg(cx.theme().background)
        .border_1()
        .border_color(cx.theme().border)
        .children(files)
        .children(loading.then(|| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Loading stashed files…")
        }))
        .overflow_y_scrollbar()
}

pub fn review_stash_file(
    index: usize,
    file: &GitFile,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Button {
    let theme = cx.theme().colors;
    let path = file.path.clone();
    let id = format!("stash-file-{index}-{path}");
    let status = file.status_char();
    let color = match status {
        'A' | '?' => theme.success,
        'D' => theme.danger,
        _ => theme.warning,
    };
    Button::new(SharedString::from(id.clone()))
        .debug_selector(move || id.clone())
        .accessibility_label(format!(
            "Review stashed file {path}, status {status}, {} additions, {} deletions",
            file.additions, file.deletions
        ))
        .ghost()
        .flex_none()
        .h_auto()
        .w_full()
        .min_w_0()
        .p_0()
        .on_click(on_open)
        .child(
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .h(rems(STASH_FILE_HEIGHT))
                .px_2()
                .rounded_sm()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(color)
                                .child(status.to_string()),
                        )
                        .child(
                            div()
                                .debug_selector(|| "review-stash-file-path".into())
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme.foreground)
                                .child(path),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "review-stash-file-stats".into())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .child(
                            div()
                                .text_color(theme.success)
                                .child(format!("+{}", file.additions)),
                        )
                        .child(
                            div()
                                .text_color(theme.danger)
                                .child(format!("-{}", file.deletions)),
                        ),
                ),
        )
}
