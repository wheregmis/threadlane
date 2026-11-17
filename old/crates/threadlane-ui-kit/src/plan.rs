//! Session-scoped plan disclosure shared by every conversation host.
use crate::truncate_preview_text;
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Sizable};
use threadlane_protocol::{PlanItemStatus, SessionPlan};
use threadlane_ui_theme::CHAT_CONTENT_MAX_WIDTH;

pub fn plan_tracker_texts(
    completed: usize,
    total: usize,
    current_step: Option<&str>,
) -> (String, String, String) {
    match current_step {
        Some(step) => (
            step.to_string(),
            format!("Task plan, {completed} of {total} complete, current step: {step}"),
            format!("Show task plan · {step}"),
        ),
        None => (
            "Complete".to_string(),
            format!("Task plan, {completed} of {total} complete"),
            "Show task plan · all steps complete".to_string(),
        ),
    }
}

/// Renders the 16px status circle used for a plan step: a bordered ✓ for
/// completed, a spinner for in-progress (active generation), a static dot for in-progress (idle), and an empty ring for pending.
fn plan_step_marker(
    status: PlanItemStatus,
    is_generating: bool,
    colors: gpui_component::ThemeColor,
) -> AnyElement {
    match status {
        PlanItemStatus::Completed => div()
            .size_4()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(colors.success)
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(colors.success)
            .child("✓")
            .into_any_element(),
        PlanItemStatus::InProgress => {
            if is_generating {
                div()
                    .size_4()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(colors.primary)
                    .child(gpui_component::spinner::Spinner::new().xsmall())
                    .into_any_element()
            } else {
                div()
                    .size_4()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(colors.primary)
                    .child(div().size(rems(0.375)).rounded_full().bg(colors.primary))
                    .into_any_element()
            }
        }
        PlanItemStatus::Pending => div()
            .size_4()
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(colors.muted_foreground)
            .into_any_element(),
    }
}

pub fn plan_tracker(
    session_id: &str,
    plan: &SessionPlan,
    is_generating: bool,
    cx: &App,
) -> Option<AnyElement> {
    if plan.items.is_empty() {
        return None;
    }

    let theme = cx.theme().colors;
    let completed = plan
        .items
        .iter()
        .filter(|item| item.status == PlanItemStatus::Completed)
        .count();
    let total = plan.items.len();
    let current_step_opt = plan
        .items
        .iter()
        .position(|item| item.status == PlanItemStatus::InProgress)
        .or_else(|| {
            plan.items
                .iter()
                .position(|item| item.status == PlanItemStatus::Pending)
        })
        .map(|index| plan.items[index].step.as_str());
    let (display_step, tracker_a11y, tracker_tooltip) =
        plan_tracker_texts(completed, total, current_step_opt);
    let content_plan = plan.clone();

    Some(
        div()
            .w_full()
            .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
            .mx_auto()
            .px_4()
            .min_w_0()
            .flex()
            .justify_center()
            .py_1()
            .child(
                Popover::new(SharedString::from(format!("session-plan-{}", session_id)))
                    .anchor(Anchor::BottomCenter)
                    .appearance(false)
                    .trigger(
                        Button::new("session-plan-tracker")
                            .debug_selector(|| "session-plan-tracker".into())
                            .secondary()
                            .small()
                            .rounded_full()
                            .label(format!(
                                "Plan · {completed}/{total} · {}",
                                truncate_preview_text(&display_step, 42)
                            ))
                            .accessibility_label(tracker_a11y.clone())
                            .tooltip(tracker_tooltip.clone())
                            .max_w(rems(26.0))
                            .min_w_0()
                            .flex_shrink_1()
                            .overflow_hidden(),
                    )
                    .content(move |_state, window, _cx| {
                        let colors = theme;
                        let rows = content_plan.items.iter().enumerate().map(|(index, item)| {
                            let marker = plan_step_marker(item.status, is_generating, colors);
                            div().flex().items_start().gap_2().child(marker).child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(colors.foreground)
                                    .child(format!("{}. {}", index + 1, item.step)),
                            )
                        });
                        div()
                            .debug_selector(|| "session-plan-details".into())
                            .w(rems(32.0))
                            .max_w(window.viewport_size().width - window.rem_size() * 2.0)
                            .p_3()
                            .rounded_xl()
                            .border_1()
                            .border_color(colors.border.opacity(0.8))
                            .bg(colors.popover)
                            .shadow_lg()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(content_plan.explanation.clone().map(|explanation| {
                                div()
                                    .flex_none()
                                    .pb_2()
                                    .border_b_1()
                                    .border_color(colors.border.opacity(0.4))
                                    .text_sm()
                                    .text_color(colors.muted_foreground)
                                    .child(explanation)
                            }))
                            .child(
                                div()
                                    .w_full()
                                    .max_h(rems(18.0))
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .overflow_y_scrollbar()
                                    .children(rows),
                            )
                    }),
            )
            .into_any_element(),
    )
}
