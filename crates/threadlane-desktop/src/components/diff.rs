//! Collapsible file-stat and unified diff preview for publish decisions and
//! the task `Changes` panel.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::workspace::DiffPreview;
use threadlane_desktop::{count_form, format_count};

use super::{button, meta};
use crate::Desktop;
use crate::icons::AppIcon;
use crate::tokens::{BADGE_RADIUS, MD, ROW_RADIUS, SM, Status, TABULAR, TYPE_META, XS, XXS};

/// Max height of the expanded unified diff viewport before scrolling.
const DIFF_MAX_HEIGHT: Rems = rems(17.5);

impl Desktop {
    /// Renders a compact file-stat summary with an expandable unified diff
    /// preview using theme semantic status tokens.
    pub(crate) fn diff_preview(&self, id: String, diff: &DiffPreview, cx: &Context<Self>) -> Div {
        let expanded = self.state.diff_expanded(&id);
        let toggle_id = id.clone();
        let (add_ink, add_tint) = Status::Done.colors(cx.theme());
        let (del_ink, del_tint) = Status::Failed.colors(cx.theme());
        let (hunk_ink, hunk_tint) = Status::Running.colors(cx.theme());
        let mono = cx.theme().mono_font_family.clone();

        let file_count = diff.files.len();
        let files_label = format!(
            "{} {} changed",
            format_count(file_count as u128),
            count_form(file_count, "file", "files")
        );
        let has_patch = !diff.patch.is_empty();

        let header = h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap(SM)
            .flex_wrap()
            .child(
                h_flex()
                    .gap(SM)
                    .items_center()
                    .flex_wrap()
                    .child(
                        Icon::new(AppIcon::FileText)
                            .size(TYPE_META)
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_size(TYPE_META)
                            .font_weight(FontWeight::MEDIUM)
                            .font_features(TABULAR.clone())
                            .child(files_label),
                    )
                    .child(
                        div()
                            .text_size(TYPE_META)
                            .text_color(add_ink)
                            .font_family(mono.clone())
                            .font_features(TABULAR.clone())
                            .child(format!("+{}", format_count(u128::from(diff.insertions)))),
                    )
                    .child(
                        div()
                            .text_size(TYPE_META)
                            .text_color(del_ink)
                            .font_family(mono.clone())
                            .font_features(TABULAR.clone())
                            .child(format!("-{}", format_count(u128::from(diff.deletions)))),
                    ),
            )
            .children(has_patch.then(|| {
                button(SharedString::from(format!("toggle-diff-{id}")))
                    .ghost()
                    .xsmall()
                    .icon(if expanded {
                        AppIcon::ChevronDown
                    } else {
                        AppIcon::ChevronRight
                    })
                    .label(if expanded { "Hide diff" } else { "Show diff" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.toggle_diff_expanded(toggle_id.clone());
                        cx.notify();
                    }))
            }));

        let mut file_rows = v_flex().gap(XXS);
        let visible_files = if expanded {
            diff.files.len()
        } else {
            diff.files.len().min(6)
        };
        for file in &diff.files[..visible_files] {
            let stats = if file.binary {
                meta("binary", cx)
            } else {
                h_flex()
                    .gap(XS)
                    .flex_none()
                    .font_family(mono.clone())
                    .font_features(TABULAR.clone())
                    .text_size(TYPE_META)
                    .child(
                        div()
                            .text_color(add_ink)
                            .child(format!("+{}", file.insertions)),
                    )
                    .child(
                        div()
                            .text_color(del_ink)
                            .child(format!("-{}", file.deletions)),
                    )
            };
            file_rows = file_rows.child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap(SM)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(TYPE_META)
                            .font_family(mono.clone())
                            .text_color(cx.theme().foreground)
                            .child(file.path.clone()),
                    )
                    .child(stats),
            );
        }
        if !expanded && diff.files.len() > visible_files {
            file_rows = file_rows.child(meta(
                format!("…and {} more", diff.files.len() - visible_files),
                cx,
            ));
        }

        let mut container = v_flex()
            .w_full()
            .min_w_0()
            .gap(XS)
            .p(SM)
            .rounded(ROW_RADIUS)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(header)
            .child(file_rows);

        if expanded && has_patch {
            let mut lines = v_flex()
                .w_full()
                .min_w_0()
                .font_family(mono)
                .text_size(TYPE_META);
            for line in diff.patch.lines() {
                let (bg, fg) = if line.starts_with("+++") || line.starts_with("---") {
                    (None, cx.theme().muted_foreground)
                } else if line.starts_with('+') {
                    (Some(add_tint), add_ink)
                } else if line.starts_with('-') {
                    (Some(del_tint), del_ink)
                } else if line.starts_with("@@") {
                    (Some(hunk_tint), hunk_ink)
                } else if line.starts_with("diff --git") {
                    (None, cx.theme().foreground)
                } else {
                    (None, cx.theme().muted_foreground)
                };
                let mut row = div()
                    .w_full()
                    .px(XS)
                    .text_color(fg)
                    .whitespace_normal()
                    .child(if line.is_empty() {
                        " ".to_owned()
                    } else {
                        line.to_owned()
                    });
                if let Some(bg) = bg {
                    row = row.bg(bg);
                }
                lines = lines.child(row);
            }
            let patch_box = div()
                .id(SharedString::from(format!("diff-patch-{id}")))
                .w_full()
                .max_h(DIFF_MAX_HEIGHT)
                .overflow_y_scrollbar()
                .rounded(BADGE_RADIUS)
                .border_1()
                .border_color(cx.theme().border)
                .py(XS)
                .child(lines);
            container = container.child(patch_box);
            if diff.truncated {
                container =
                    container.child(meta("Diff preview clipped to keep the card compact.", cx));
            }
        }

        let _ = MD;
        container
    }
}
