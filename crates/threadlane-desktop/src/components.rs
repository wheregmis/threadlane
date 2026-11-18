//! Common desktop surfaces and controls, built on GPUI Kit.

use gpui_kit::component::badge::Badge;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    App, Div, ElementId, Entity, FontWeight, ParentElement as _, SharedString, Styled as _, div,
};

pub(crate) mod chat_images;
pub(crate) mod composer;
mod prose;

use crate::icons::AppIcon;
use crate::tokens::{
    BADGE_RADIUS, BAR_HEIGHT, BUTTON_RADIUS, COMPOSER_RADIUS, CONTROL_HEIGHT, DOT_SIZE,
    FIELD_RADIUS, ICON_BUTTON_RADIUS, MD, ROW_HEIGHT, ROW_RADIUS, SM, SMALL_CONTROL_HEIGHT, Status,
    TABULAR, TYPE_BODY, TYPE_META, TYPE_SECTION, TYPE_TITLE, XS, XXL, XXS,
};

/// Standard push button. The kit's medium and small heights match our scale.
pub(super) fn button(id: impl Into<ElementId>) -> Button {
    Button::new(id).rounded(BUTTON_RADIUS)
}

/// A named and tooltip-labelled icon-only command.
pub(super) fn icon_button(id: impl Into<ElementId>, icon: AppIcon, name: String) -> Button {
    button(id)
        .ghost()
        .small()
        .size(SMALL_CONTROL_HEIGHT)
        .rounded(ICON_BUTTON_RADIUS)
        .icon(icon)
        .accessibility_label(name.clone())
        .tooltip(name)
}

/// Standard single-line field.
pub(super) fn field(state: &Entity<InputState>) -> Input {
    Input::new(state).rounded(FIELD_RADIUS).text_size(TYPE_BODY)
}

/// Shared composer surface.
fn composer_surface(cx: &App) -> Div {
    h_flex()
        .gap(SM)
        .p(SM)
        .min_h(CONTROL_HEIGHT)
        .flex_wrap()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .rounded(COMPOSER_RADIUS)
}

/// A kit Badge anchored on a small tinted circle.
pub(super) fn status_dot(status: Status, cx: &App) -> Div {
    let (ink, tint) = status.colors(cx.theme());
    h_flex()
        .justify_center()
        .flex_none()
        .size(TYPE_META)
        .rounded_full()
        .bg(tint)
        .child(Badge::new().dot().color(ink).child(div().size(DOT_SIZE)))
}

fn status_icon(status: Status) -> AppIcon {
    match status {
        Status::Running => AppIcon::Play,
        Status::Waiting => AppIcon::Clock,
        Status::Done => AppIcon::Check,
        Status::Failed => AppIcon::X,
        Status::Idle => AppIcon::CircleDashed,
    }
}

/// A count badge for nonempty collections.
pub(super) fn count(value: usize, cx: &App) -> Option<Tag> {
    (value != 0).then(|| count_badge(value.to_string(), cx))
}

/// An in-flow badge when zero or a fraction carries meaning.
pub(super) fn count_badge(value: impl Into<SharedString>, cx: &App) -> Tag {
    Tag::custom(cx.theme().muted, cx.theme().foreground, cx.theme().muted)
        .small()
        .rounded(BADGE_RADIUS)
        .text_size(TYPE_META)
        .py(XXS)
        .font_features(TABULAR.clone())
        .child(div().text_right().child(value.into()))
}

/// A task or run label on the status tint for the active theme.
pub(super) fn status_badge(label: impl Into<SharedString>, status: Status, cx: &App) -> Tag {
    let (ink, tint) = status.colors(cx.theme());
    Tag::custom(tint, cx.theme().foreground, tint.opacity(0.))
        .small()
        .rounded(BADGE_RADIUS)
        .text_size(TYPE_META)
        .gap(XS)
        .font_features(TABULAR.clone())
        .child(
            Icon::new(status_icon(status))
                .size(TYPE_META)
                .text_color(ink),
        )
        .child(label.into())
}

/// Section title with an optional trailing count or action.
pub(super) fn section(text: impl Into<SharedString>) -> Div {
    h_flex()
        .justify_between()
        .gap(SM)
        .min_w_0()
        .text_size(TYPE_SECTION)
        .font_weight(FontWeight::SEMIBOLD)
        .child(div().min_w_0().child(text.into()))
}

/// Shared page header with a title, optional chips, and trailing actions.
pub(super) fn page_header(
    title: impl Into<SharedString>,
    chips: Option<Div>,
    actions: Option<Div>,
) -> Div {
    h_flex()
        .w_full()
        .min_h(BAR_HEIGHT)
        .flex_none()
        .gap(MD)
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .gap(XS)
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .text_size(TYPE_TITLE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.into()),
                )
                .children(chips),
        )
        .children(actions.map(|actions| actions.flex_none().min_h(SMALL_CONTROL_HEIGHT)))
}

/// The shared title treatment for a page without actions.
pub(super) fn heading(text: impl Into<SharedString>) -> Div {
    page_header(text, None, None)
}

/// Navigation row with a kit Button's hover, focus, and selected state.
pub(super) fn row(
    id: impl Into<ElementId>,
    icon: AppIcon,
    label: impl Into<SharedString>,
    trailing: Option<SharedString>,
    running: bool,
    selected: bool,
    cx: &App,
) -> Button {
    let label = label.into();
    let mut name = trailing
        .as_ref()
        .map_or_else(|| label.to_string(), |value| format!("{label}, {value}"));
    if running {
        name.push_str(", running");
    }
    let style = ButtonCustomVariant::new(cx)
        .foreground(cx.theme().sidebar_foreground)
        .hover(cx.theme().muted)
        .active(cx.theme().sidebar_accent);
    button(id)
        .custom(style)
        .rounded(ROW_RADIUS)
        .w_full()
        .h(ROW_HEIGHT)
        .selected(selected)
        .accessibility_label(name)
        .tooltip(label.clone())
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap(SM)
                .child(Icon::new(icon).size(TYPE_SECTION))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .children(running.then(|| status_dot(Status::Running, cx)))
                .children(trailing.map(|value| count_badge(value, cx))),
        )
}

/// A compact title and elapsed time, with status kept adjacent to the title.
pub(super) fn work_label(title: String, elapsed: Option<String>, status: Status, cx: &App) -> Div {
    h_flex()
        .w_full()
        .gap(XS)
        .min_w_0()
        .child(status_dot(status, cx))
        // The elapsed time goes under the title: beside it, it squeezed a narrow card's title to a few letters.
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().whitespace_normal().child(title))
                .children(elapsed.map(|elapsed| meta(elapsed, cx))),
        )
}

/// The active task's latest step or quiet warning.
pub(super) fn live_step(step: threadlane_desktop::LatestStep, cx: &App) -> Div {
    let line = meta(step.text, cx);
    if step.quiet {
        line.text_color(Status::Waiting.colors(cx.theme()).0)
    } else {
        line
    }
}

/// The shared bordered surface for groups and cards.
pub(super) fn card(cx: &App) -> GroupBox {
    GroupBox::new()
        .min_w_0()
        .rounded(ROW_RADIUS)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().group_box)
        .p(MD)
        .content_style(v_flex().gap(SM).style().clone())
}

/// Muted secondary copy.
pub(super) fn meta(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .min_w_0()
        .text_size(TYPE_META)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// A quiet sentence and optional hint for an empty region.
pub(super) fn empty_state(sentence: impl Into<SharedString>, hint: Option<&str>, cx: &App) -> Div {
    v_flex()
        .gap(XS)
        .py(XXL)
        .text_color(cx.theme().muted_foreground)
        .child(div().text_size(TYPE_BODY).child(sentence.into()))
        .children(hint.map(|hint| meta(hint.to_owned(), cx)))
}

/// A decision surface; wrap the filled surface in [`decision_ring`].
pub(super) fn decision_surface(proposal: bool, cx: &App) -> Div {
    use gpui_kit::prelude::FluentBuilder as _;

    v_flex()
        .w_full()
        .min_w_0()
        .flex_none()
        .rounded(ROW_RADIUS)
        .border_1()
        .border_color(cx.theme().border)
        .when(proposal, |surface| surface.border_dashed())
        .bg(cx.theme().group_box)
        .p(MD)
        .gap(SM)
}

/// The keyboard selection ring around a filled decision surface.
pub(super) fn decision_ring(surface: Div, current: bool, cx: &App) -> Div {
    v_flex()
        .w_full()
        .flex_none()
        .p(XXS)
        .rounded(ROW_RADIUS + XXS)
        .border_1()
        .border_color(if current {
            cx.theme().ring
        } else {
            cx.theme().ring.opacity(0.)
        })
        .child(surface)
}

/// A key displayed on an option or in the keyboard shortcuts dialog.
pub(super) fn key_hint(label: impl Into<SharedString>, cx: &App) -> Div {
    meta(label, cx)
        .flex_none()
        .rounded(crate::tokens::KEY_RADIUS)
        .bg(cx.theme().muted)
        .px(XS)
        .py(XXS)
        .font_family(cx.theme().mono_font_family.clone())
}

/// The question is the main text of the decision.
pub(super) fn decision_title(question: String) -> Div {
    div()
        .text_size(crate::tokens::TYPE_EMPHASIS)
        .font_weight(FontWeight::SEMIBOLD)
        .min_w_0()
        .child(question)
}

/// A full-width option with space for wrapped text and a trailing key.
pub(super) fn decision_option(
    id: impl Into<ElementId>,
    label: String,
    recommended: bool,
    chosen: bool,
    dimmed: bool,
    key: Option<String>,
    cx: &App,
) -> Button {
    use gpui_kit::prelude::FluentBuilder as _;

    button(id)
        .outline()
        .size(CONTROL_HEIGHT)
        .w_full()
        .min_h(CONTROL_HEIGHT)
        .py(XS)
        .tab_stop(true)
        .accessibility_label(if recommended {
            format!("{label}, Recommended")
        } else {
            label.clone()
        })
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap(SM)
                .when(dimmed, |row| row.opacity(crate::tokens::UNCHOSEN_OPACITY))
                .when(chosen, |row| row.text_color(cx.theme().foreground))
                .when(chosen, |row| {
                    row.child(Icon::new(AppIcon::Check).size(TYPE_BODY).flex_none())
                })
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .flex_wrap()
                        .gap(XS)
                        .child(div().min_w_0().whitespace_normal().child(label))
                        .when(recommended, |row| row.child(count_badge("Recommended", cx))),
                )
                .children(key.map(|label| key_hint(label, cx))),
        )
}

/// A decision screen's vertical scrolling region and its matching scrollbar.
pub(super) fn decision_scroller(
    id: &'static str,
    content: Div,
    handle: &gpui_kit::ScrollHandle,
) -> Div {
    use gpui_kit::component::InteractiveElementExt as _;
    use gpui_kit::component::scroll::ScrollableElement as _;
    use gpui_kit::{InteractiveElement as _, StatefulInteractiveElement as _};

    div()
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            div()
                .id(id)
                .size_full()
                .overflow_y_scroll()
                .lock_scroll_axis()
                .track_scroll(handle)
                .child(content.w_full().min_h_full()),
        )
        .vertical_scrollbar(handle)
}
