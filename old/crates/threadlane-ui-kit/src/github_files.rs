//! Controlled PR file review surfaces. Hosts own requests and viewed-marker guards.
use super::detail_content;
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::spinner::Spinner;
use gpui_component::text::TextViewState;
use gpui_component::{ActiveTheme, Disableable, Sizable};
use std::rc::Rc;
use threadlane_protocol::repo::GitHubPrFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrFileAction {
    Previous,
    Next,
    Open,
}

pub fn pr_file_action_ix(
    current: Option<usize>,
    len: usize,
    action: PrFileAction,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = current.unwrap_or_default().min(len - 1);
    Some(match action {
        PrFileAction::Previous => current.saturating_sub(1),
        PrFileAction::Next => current.saturating_add(1).min(len - 1),
        PrFileAction::Open => current,
    })
}

pub fn github_pr_file_row(
    file: &GitHubPrFile,
    selected: bool,
    marker: Option<&str>,
    select: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let id = format!("github-pr-file-{}", file.path);
    let marker = marker.map(|text| format!(" · {text}")).unwrap_or_default();
    let theme = cx.theme().colors;
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || id.clone())
        .role(Role::ListItem)
        .aria_label(format!(
            "{}: +{} −{}{marker}",
            file.path, file.additions, file.deletions
        ))
        .aria_selected(selected)
        .w_full()
        .min_w_0()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme.border)
        .bg(if selected {
            theme.list_active
        } else {
            theme.background
        })
        .hover(|style| style.bg(theme.list_hover))
        .on_click(select)
        .child(
            div()
                .id(SharedString::from(format!("pr-file-path-{}", file.path)))
                .text_sm()
                .truncate()
                .tooltip({
                    let path = file.path.clone();
                    move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(path.clone()).build(window, cx)
                    }
                })
                .child(file.path.clone()),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .whitespace_normal()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "+{} −{} · {}{marker}",
                    file.additions, file.deletions, file.change_type
                )),
        )
}

/// Host action bindings attach to this keyboard-focusable, virtual-list region.
pub fn github_pr_file_list(focus: &FocusHandle, cx: &App) -> Stateful<Div> {
    div()
        .id("github-pr-file-list")
        .debug_selector(|| "github-pr-file-list".into())
        .role(Role::List)
        .aria_label("Changed pull request files")
        .relative()
        .w_64()
        .max_w(relative(0.4))
        .flex_shrink_0()
        .min_h_0()
        .border_1()
        .border_color(cx.theme().border)
        .track_focus(focus)
        .focus_visible(|style| style.border_color(cx.theme().ring))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubFileReviewAction {
    SetViewed(bool),
    NextUnviewed,
}

pub struct GitHubPrFilesControls {
    progress: String,
    viewed: Option<(bool, bool, SharedString)>,
    next: Option<SharedString>,
    error: Option<String>,
}
impl GitHubPrFilesControls {
    pub fn new(progress: String, next: Option<SharedString>) -> Self {
        Self {
            progress,
            viewed: None,
            next,
            error: None,
        }
    }
    pub fn viewed(mut self, checked: bool, enabled: bool, help: SharedString) -> Self {
        self.viewed = Some((checked, enabled, help));
        self
    }
    pub fn error(mut self, error: Option<String>) -> Self {
        self.error = error;
        self
    }
    pub fn render(
        self,
        request: impl Fn(GitHubFileReviewAction, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> Div {
        let request = Rc::new(request);
        let checkbox_request = request.clone();
        let next_help = self
            .next
            .clone()
            .unwrap_or_else(|| "No other unviewed files".into());
        div()
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                detail_content()
                    .debug_selector(|| "github-pr-files-toolbar".into())
                    .py_2()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_3()
                    .gap_y_1()
                    .child(
                        div()
                            .debug_selector(|| "github-pr-files-progress".into())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.progress),
                    )
                    .children(self.viewed.map(|(checked, enabled, help)| {
                        Checkbox::new("github-pr-viewed")
                            .debug_selector(|| "github-pr-viewed".into())
                            .accessibility_label(help.clone())
                            .label("Viewed")
                            .checked(checked)
                            .disabled(!enabled)
                            .small()
                            .tooltip(help)
                            .on_click(move |checked, window, cx| {
                                checkbox_request(
                                    GitHubFileReviewAction::SetViewed(*checked),
                                    window,
                                    cx,
                                )
                            })
                    }))
                    .child(div().flex_1())
                    .child(
                        Button::new("github-pr-next-unviewed")
                            .debug_selector(|| "github-pr-next-unviewed".into())
                            .ghost()
                            .small()
                            .label("Next unviewed")
                            .disabled(self.next.is_none())
                            .accessibility_label(next_help.clone())
                            .tooltip(next_help)
                            .on_click(move |_, window, cx| {
                                request(GitHubFileReviewAction::NextUnviewed, window, cx)
                            }),
                    )
                    .children(self.error.map(|error| {
                        div()
                            .w_full()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(cx.theme().danger)
                            .child(error)
                    })),
            )
    }
}

pub fn github_pr_diff(
    body: &Entity<TextViewState>,
    loading: bool,
    error: Option<String>,
    retry: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    div()
        .debug_selector(|| "github-pr-diff".into())
        .min_w_0()
        .min_h_0()
        .flex_1()
        .flex()
        .flex_col()
        .children(loading.then(|| {
            div()
                .p_4()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Spinner::new().xsmall())
                .child("Loading diff…")
        }))
        .children(error.clone().map(|error| {
            div()
                .p_4()
                .text_sm()
                .whitespace_normal()
                .text_color(cx.theme().danger)
                .child(error)
                .child(
                    div().mt_2().flex().child(
                        Button::new("github-pr-diff-retry")
                            .debug_selector(|| "github-pr-diff-retry".into())
                            .small()
                            .label("Retry diff")
                            .on_click(retry),
                    ),
                )
        }))
        .children(error.is_none().then(|| {
            crate::diff_text_view(body, cx)
                .scrollable(true)
                .size_full()
                .p_4()
        }))
}

pub fn github_pr_files(toolbar: AnyElement, files: AnyElement, diff: AnyElement) -> Div {
    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .child(toolbar)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(files)
                .child(diff),
        )
}
