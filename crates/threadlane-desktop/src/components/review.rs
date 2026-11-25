//! The live pull request and CI strip on task headers and cards, and the
//! full-width review of a task's changed files.

use std::sync::Arc;

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::github::{CheckStatus, PullRequestStatus};
use threadlane_core::ids::TaskId;
use threadlane_desktop::{
    DiffLine, LineKind, ReviewFile, check_counts, check_label, pull_label, quote, side_by_side,
};

use super::{button, card, icon_button, meta, section, status_badge};
use crate::Desktop;
use crate::icons::AppIcon;
use crate::tokens::{ROW_RADIUS, SM, Status, TABULAR, TYPE_META, XS, XXS};

/// The file list's width beside the diff.
const FILE_LIST_WIDTH: Rems = rems(15.);
/// The width of one line-number gutter.
const GUTTER_WIDTH: Rems = rems(2.75);

fn pull_status(status: PullRequestStatus) -> Status {
    match status {
        PullRequestStatus::Open => Status::Running,
        PullRequestStatus::Draft => Status::Idle,
        PullRequestStatus::Merged => Status::Done,
        PullRequestStatus::Closed => Status::Failed,
    }
}

fn check_status(status: CheckStatus) -> Status {
    match status {
        CheckStatus::Passing => Status::Done,
        CheckStatus::Running => Status::Running,
        CheckStatus::Failing => Status::Failed,
    }
}

impl Desktop {
    /// Each pull request of task `task` with its state, check pills and a
    /// conflict warning; `None` while it has none.
    pub(crate) fn pull_strip(&self, space: &str, task: TaskId, cx: &App) -> Option<Div> {
        let pulls = self.state.task_pulls(space, task);
        if pulls.is_empty() {
            return None;
        }
        let mut strip = h_flex().gap(XS).flex_wrap().items_center().min_w_0();
        for pull in pulls {
            let names = pull
                .checks
                .iter()
                .map(|check| check.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            strip = strip
                .child(
                    div()
                        .id(SharedString::from(format!("pull-{task}-{}", pull.number)))
                        .child(status_badge(pull_label(pull), pull_status(pull.status), cx)),
                )
                .children(check_counts(&pull.checks).into_iter().map(|(status, n)| {
                    div()
                        .id(SharedString::from(format!(
                            "checks-{task}-{}-{status:?}",
                            pull.number
                        )))
                        .tooltip({
                            let names = names.clone();
                            move |window, cx| Tooltip::new(names.clone()).build(window, cx)
                        })
                        .child(status_badge(
                            check_label(status, n),
                            check_status(status),
                            cx,
                        ))
                }))
                .children(pull.conflicted.then(|| {
                    status_badge("Conflicts with base", Status::Failed, cx)
                        .text_color(cx.theme().danger)
                }));
        }
        Some(strip)
    }

    /// Opens the full-width review of task `task` and reads its pull
    /// requests' files.
    pub(crate) fn open_review(&mut self, space: String, task: TaskId, cx: &mut Context<Self>) {
        self.state.open_review(&space, task);
        self.load_review_files(space, task, cx);
        cx.notify();
    }

    /// Reads the files of task `task`'s pull requests for its review.
    pub(crate) fn load_review_files(
        &mut self,
        space: String,
        task: TaskId,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        self.run(
            async move { threadlane_desktop::load_pull_files(&*core.read().await, &space, task).await },
            |this, loaded, _| this.state.apply_pull_files(loaded),
            cx,
        );
    }

    /// The full-width review: a file list, the chosen file's diff unified
    /// or side by side, and a file or hunk header that quotes into the
    /// task composer.
    pub(crate) fn review_view(&self, space: &str, task: TaskId, cx: &mut Context<Self>) -> Div {
        let files = self.state.review_files(space, task);
        let Some(review) = self.state.review(space, task) else {
            return div();
        };
        let selected = review.file.min(files.len().saturating_sub(1));
        let split = review.split;
        let count = files.len();

        let toolbar = h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap(SM)
            .flex_wrap()
            .child(section(format!(
                "Review · {count} {}",
                threadlane_desktop::count_form(count, "file", "files")
            )))
            .child(
                h_flex()
                    .gap(SM)
                    .flex_wrap()
                    .items_center()
                    .child(
                        ButtonGroup::new("review-layout")
                            .child(
                                button("review-unified")
                                    .small()
                                    .outline()
                                    .label("Unified")
                                    .selected(!split)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if split {
                                            this.state.toggle_review_split();
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                button("review-split")
                                    .small()
                                    .outline()
                                    .label("Side by side")
                                    .selected(split)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !split {
                                            this.state.toggle_review_split();
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        icon_button(
                            "review-previous",
                            AppIcon::ArrowLeft,
                            "Previous file".into(),
                        )
                        .tab_stop(true)
                        .disabled(selected == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.select_review_file(selected.saturating_sub(1));
                            cx.notify();
                        })),
                    )
                    .child(
                        icon_button("review-next", AppIcon::ArrowRight, "Next file".into())
                            .tab_stop(true)
                            .disabled(selected + 1 >= count)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.select_review_file(selected + 1);
                                cx.notify();
                            })),
                    )
                    .child(
                        button("review-request-changes")
                            .small()
                            .primary()
                            .icon(AppIcon::Send)
                            .label("Request changes")
                            .accessibility_label(format!(
                                "Send the quoted changes to {task} so its agent goes back to work"
                            ))
                            .disabled(!self.task_draft_ready(space, task))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit_draft(window, cx);
                            })),
                    )
                    .child(
                        icon_button("review-close", AppIcon::X, "Close review".into())
                            .tab_stop(true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.close_review();
                                cx.notify();
                            })),
                    ),
            );

        let mut view = v_flex().w_full().min_w_0().gap(SM).child(toolbar);
        if let Some(note) = self.state.pull_files_note(space, task) {
            view = view.child(meta(note, cx));
        }
        if files.is_empty() {
            return view.child(meta("No changed files to show yet.", cx));
        }
        view = view.child(meta(
            "Click a file or hunk header to quote it into the message below.",
            cx,
        ));

        let (add_ink, _) = Status::Done.colors(cx.theme());
        let (del_ink, _) = Status::Failed.colors(cx.theme());
        let mono = cx.theme().mono_font_family.clone();
        let mut list = v_flex().gap(XXS);
        for (index, file) in files.iter().enumerate() {
            list = list.child(
                button(SharedString::from(format!("review-file-{index}")))
                    .ghost()
                    .small()
                    .w_full()
                    .selected(index == selected)
                    .accessibility_label(format!(
                        "{} in {}, {} added, {} removed",
                        file.path, file.repository, file.additions, file.deletions
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.select_review_file(index);
                        cx.notify();
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap(XS)
                            .justify_between()
                            .text_size(TYPE_META)
                            .font_family(mono.clone())
                            .child(div().flex_1().min_w_0().truncate().child(file.path.clone()))
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(XS)
                                    .font_features(TABULAR.clone())
                                    .child(
                                        div()
                                            .text_color(add_ink)
                                            .child(format!("+{}", file.additions)),
                                    )
                                    .child(
                                        div()
                                            .text_color(del_ink)
                                            .child(format!("-{}", file.deletions)),
                                    ),
                            ),
                    ),
            );
        }
        let list = card(cx)
            .p(XS)
            .w(FILE_LIST_WIDTH)
            .max_w_full()
            .flex_none()
            .child(list);

        let file = &files[selected];
        view.child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .flex_wrap()
                .gap(SM)
                .child(list)
                .child(self.file_diff(file, split, cx)),
        )
    }

    fn file_diff(&self, file: &ReviewFile, split: bool, cx: &mut Context<Self>) -> Div {
        let mono = cx.theme().mono_font_family.clone();
        let quoted = quote(file, None);
        let mut body = v_flex()
            .flex_1()
            .min_w_0()
            .rounded(ROW_RADIUS)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .font_family(mono)
            .text_size(TYPE_META)
            .child(
                button("review-quote-file")
                    .ghost()
                    .small()
                    .w_full()
                    .icon(AppIcon::FileText)
                    .label(file.path.clone())
                    .accessibility_label(format!(
                        "Quote {} in {} into the message",
                        file.path, file.repository
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.quote_to_composer(&quoted, window, cx);
                    })),
            );
        if file.hunks.is_empty() {
            return body.child(div().p(SM).child(meta(
                "No diff to show: the file is binary or too large.",
                cx,
            )));
        }
        let (hunk_ink, hunk_tint) = Status::Running.colors(cx.theme());
        for (index, hunk) in file.hunks.iter().enumerate() {
            let quoted = quote(file, Some(hunk));
            body = body.child(
                button(SharedString::from(format!("review-hunk-{index}")))
                    .ghost()
                    .small()
                    .w_full()
                    .bg(hunk_tint)
                    .text_color(hunk_ink)
                    .label(hunk.header.clone())
                    .accessibility_label(format!("Quote hunk {} into the message", hunk.header))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.quote_to_composer(&quoted, window, cx);
                    })),
            );
            if split {
                for (old, new) in side_by_side(hunk) {
                    body = body.child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_start()
                            .child(diff_cell(old, true, cx))
                            .child(div().w(px(1.)).self_stretch().bg(cx.theme().border))
                            .child(diff_cell(new, false, cx)),
                    );
                }
            } else {
                for line in &hunk.lines {
                    body = body.child(unified_row(line, cx));
                }
            }
        }
        body
    }
}

fn line_colors(kind: LineKind, cx: &App) -> (Option<Hsla>, Hsla) {
    match kind {
        LineKind::Added => {
            let (ink, tint) = Status::Done.colors(cx.theme());
            (Some(tint), ink)
        }
        LineKind::Removed => {
            let (ink, tint) = Status::Failed.colors(cx.theme());
            (Some(tint), ink)
        }
        LineKind::Context => (None, cx.theme().foreground),
    }
}

fn gutter(number: Option<u32>, cx: &App) -> Div {
    div()
        .w(GUTTER_WIDTH)
        .flex_none()
        .pr(XS)
        .text_right()
        .font_features(TABULAR.clone())
        .text_color(cx.theme().muted_foreground)
        .child(number.map(|n| n.to_string()).unwrap_or_default())
}

fn text(line: &str) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .whitespace_normal()
        .child(if line.is_empty() {
            " ".to_owned()
        } else {
            line.to_owned()
        })
}

fn unified_row(line: &DiffLine, cx: &App) -> Div {
    let (bg, fg) = line_colors(line.kind, cx);
    let sign = match line.kind {
        LineKind::Context => " ",
        LineKind::Added => "+",
        LineKind::Removed => "-",
    };
    h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .when_some(bg, |row, bg| row.bg(bg))
        .child(gutter(line.old, cx))
        .child(gutter(line.new, cx))
        .child(div().w_3().flex_none().text_color(fg).child(sign))
        .child(text(&line.text).text_color(fg))
}

fn diff_cell(line: Option<&DiffLine>, old: bool, cx: &App) -> Div {
    let cell = h_flex().flex_1().min_w_0().items_start();
    let Some(line) = line else {
        return cell.bg(cx.theme().muted);
    };
    let (bg, fg) = line_colors(line.kind, cx);
    cell.when_some(bg, |cell, bg| cell.bg(bg))
        .child(gutter(if old { line.old } else { line.new }, cx))
        .child(text(&line.text).text_color(fg))
}
