//! Controlled `@` picker. Hosts own discovery, selection, and guarded insertion.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon, Sizable};
use std::rc::Rc;

#[path = "file_completion_query.rs"]
mod query;
pub use query::*;

pub const FILE_COMPLETION_KEY_CONTEXT: &str = "FileCompletionMenu";
pub const FILE_COMPLETION_BINDING_CONTEXT: &str = "FileCompletionMenu > Input";

actions!(
    threadlane_file_completion,
    [
        CompleteFileCompletion,
        SelectPreviousFileCompletion,
        SelectNextFileCompletion,
        DismissFileCompletion,
    ]
);

pub fn init_file_completion(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "tab",
            CompleteFileCompletion,
            Some(FILE_COMPLETION_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "up",
            SelectPreviousFileCompletion,
            Some(FILE_COMPLETION_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "down",
            SelectNextFileCompletion,
            Some(FILE_COMPLETION_BINDING_CONTEXT),
        ),
        KeyBinding::new(
            "escape",
            DismissFileCompletion,
            Some(FILE_COMPLETION_BINDING_CONTEXT),
        ),
    ]);
}

#[derive(Clone, Copy)]
pub enum FileCompletionMenuStatus<'a> {
    Loading,
    Unavailable(&'a str),
    Failed(&'a str),
    Ready {
        matches: &'a [String],
        selected_index: usize,
        has_more: bool,
        non_utf8_skipped: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileCompletionRequest {
    Insert(String),
    Retry,
}

/// The same surface is positioned above the composer or shown inline in the gallery.
pub fn file_completion_popup(menu: impl IntoElement) -> Div {
    div()
        .absolute()
        .bottom_full()
        .left(rems(0.0))
        .mb_2()
        .w_full()
        .min_w_0()
        .child(menu)
}

pub fn file_completion_row(path: &str, index: usize, active: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let basename = path.rsplit('/').next().unwrap_or(path);
    let parent = path.strip_suffix(basename).unwrap_or("");
    div()
        .id(SharedString::from(format!("composer-file-{index}")))
        .debug_selector(move || format!("composer-file-{index}"))
        .role(Role::Button)
        .aria_label(format!("{path} — insert this path at the caret"))
        .w_full()
        .min_w_0()
        .h(rems(1.875))
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .px_2()
        .text_sm()
        .bg(if active {
            theme.accent.opacity(0.16)
        } else {
            transparent_black()
        })
        .hover(|style| style.bg(theme.list_hover))
        .child(
            Icon::default()
                .path("icons/file.svg")
                .small()
                .flex_none()
                .text_color(if active {
                    theme.primary
                } else {
                    theme.muted_foreground
                }),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .when(!parent.is_empty(), |el| el.max_w(relative(0.6)))
                .font_weight(if active {
                    FontWeight::BOLD
                } else {
                    FontWeight::SEMIBOLD
                })
                .text_color(if active {
                    theme.primary
                } else {
                    theme.foreground
                })
                .child(basename.to_owned()),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(parent.to_owned()),
        )
}

fn status_row(text: impl Into<SharedString>, failed: bool, cx: &App) -> Stateful<Div> {
    div()
        .id("file-completion-status")
        .role(if failed { Role::Alert } else { Role::Status })
        .min_w_0()
        .min_h(rems(1.875))
        .flex()
        .items_center()
        .px_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

pub fn file_completion_menu(
    scope: &str,
    status: FileCompletionMenuStatus<'_>,
    scroll: &ScrollHandle,
    on_request: impl Fn(&FileCompletionRequest, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let on_request = Rc::new(on_request);
    let (count, selected, has_more) = match status {
        FileCompletionMenuStatus::Ready {
            matches,
            selected_index,
            has_more,
            ..
        } => (
            matches.len(),
            selected_index.min(matches.len().saturating_sub(1)),
            has_more,
        ),
        _ => (0, 0, false),
    };
    let list_label = match status {
        FileCompletionMenuStatus::Loading => format!("Searching files in {scope}"),
        FileCompletionMenuStatus::Unavailable(reason) => {
            format!("File completion unavailable: {reason}")
        }
        FileCompletionMenuStatus::Failed(error) => {
            format!("File search failed in {scope}: {error}")
        }
        FileCompletionMenuStatus::Ready {
            matches,
            non_utf8_skipped,
            ..
        } => {
            let note = if non_utf8_skipped > 0 {
                format!(", {non_utf8_skipped} non-UTF-8 names skipped")
            } else {
                String::new()
            };
            format!(
                "Files in {scope}, {} of {count}, {} selected{note}",
                if count == 0 { 0 } else { selected + 1 },
                matches.get(selected).map(String::as_str).unwrap_or("none")
            )
        }
    };
    let body = match status {
        FileCompletionMenuStatus::Loading => {
            status_row("Searching files…", false, cx).into_any_element()
        }
        FileCompletionMenuStatus::Unavailable(reason) => {
            status_row(reason.to_owned(), false, cx).into_any_element()
        }
        FileCompletionMenuStatus::Failed(error) => {
            let callback = on_request.clone();
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(status_row(format!("Could not list files: {error}"), true, cx).flex_1())
                .child(
                    Button::new("file-completion-retry")
                        .debug_selector(|| "file-completion-retry".into())
                        .label("Retry")
                        .small()
                        .ghost()
                        .accessibility_label("Retry listing workspace files")
                        .on_click(move |_, window, cx| {
                            callback(&FileCompletionRequest::Retry, window, cx)
                        }),
                )
                .into_any_element()
        }
        FileCompletionMenuStatus::Ready { matches, .. } if matches.is_empty() => {
            status_row("No matching files", false, cx).into_any_element()
        }
        FileCompletionMenuStatus::Ready { matches, .. } => div()
            .min_w_0()
            .children(matches.iter().enumerate().map(|(index, path)| {
                let callback = on_request.clone();
                let request = FileCompletionRequest::Insert(path.clone());
                file_completion_row(path, index, index == selected, cx)
                    .on_click(move |_, window, cx| callback(&request, window, cx))
            }))
            .into_any_element(),
    };
    div()
        .id("file-completion-menu")
        .debug_selector(|| "file-completion-menu".into())
        .w_full()
        .min_w_0()
        .max_w(rems(40.0))
        .max_h(rems(20.0))
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .shadow_xl()
        .p_1p5()
        .child(
            div()
                .min_w_0()
                .flex()
                .items_start()
                .justify_between()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(theme.border.opacity(0.4))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .truncate()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("Files · {scope}")),
                        )
                        .child("↑↓ navigate · Tab/Enter insert · Esc dismiss"),
                )
                .child(div().flex_none().child(if count > 0 {
                    format!("{}/{count}", selected + 1)
                } else {
                    "0/0".into()
                })),
        )
        .child(
            div()
                .id("file-completion-list")
                .debug_selector(|| "file-completion-list".into())
                .role(Role::List)
                .aria_label(list_label)
                .relative()
                .min_w_0()
                .min_h_0()
                .mt_1()
                .track_scroll(scroll)
                .overflow_y_scroll()
                .vertical_scrollbar(scroll)
                .max_h(rems(16.25))
                .child(body)
                .when(has_more, |list| {
                    list.child(status_row("More matches — keep typing", false, cx))
                }),
        )
}
