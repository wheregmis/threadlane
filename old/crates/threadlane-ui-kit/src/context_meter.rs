use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::popover::Popover;
use gpui_component::progress::ProgressCircle;
use gpui_component::ActiveTheme;
use gpui_component::Selectable;
use threadlane_protocol::daemon::SubagentActivityStatus;

pub const CONTEXT_METER_WARN_PCT: f64 = 80.0;
pub const CONTEXT_METER_DANGER_PCT: f64 = 95.0;

#[derive(Clone, Debug)]
pub struct ContextMeterContext {
    pub current_tokens: u64,
    pub context_limit: u64,
    pub context_limit_is_estimate: bool,
    pub effective_model: String,
    pub last_compaction_seq: Option<u64>,
    pub provisional: bool,
    pub estimating: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ContextMeterMetrics {
    pub billed_input_tokens: u64,
    pub output_tokens: u64,
    pub cache_hit_percent: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextMeterViewModel {
    pub percent: Option<f64>,
    pub bar_percent: f64,
    pub current_label: String,
    pub detail_label: String,
    pub total_processed_label: String,
    pub cache_hit_label: Option<String>,
    pub effective_model: Option<String>,
    pub last_compaction_seq: Option<u64>,
    pub provisional: bool,
}

pub fn subagent_popover_counts(
    statuses: impl IntoIterator<Item = SubagentActivityStatus>,
) -> Option<(usize, usize)> {
    let (count, active_count) = statuses
        .into_iter()
        .fold((0, 0), |(count, active), status| {
            (
                count + 1,
                active
                    + usize::from(matches!(
                        status,
                        SubagentActivityStatus::Queued | SubagentActivityStatus::Running
                    )),
            )
        });
    (count > 0).then_some((count, active_count))
}

pub fn format_meter_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

pub fn context_meter_view_model(
    context: Option<&ContextMeterContext>,
    metrics: &ContextMeterMetrics,
    reports_usage: bool,
) -> ContextMeterViewModel {
    let total_processed = metrics
        .billed_input_tokens
        .saturating_add(metrics.output_tokens);
    let cache_hit_label = metrics.cache_hit_percent.map(|value| format!("{value}%"));

    // An external ACP agent runs its own loop and reports no token accounting,
    // so there is no context window to measure. Saying "Estimating…" would
    // promise a number that never arrives.
    if !reports_usage {
        return ContextMeterViewModel {
            percent: None,
            bar_percent: 0.0,
            current_label: "Not reported".into(),
            detail_label: "Context usage is not reported by this agent".into(),
            total_processed_label: format_meter_tokens(total_processed),
            cache_hit_label,
            effective_model: None,
            last_compaction_seq: None,
            provisional: false,
        };
    }

    let Some(context) = context else {
        return ContextMeterViewModel {
            percent: None,
            bar_percent: 0.0,
            current_label: "Unavailable".into(),
            detail_label: "Context usage details, current usage unavailable".into(),
            total_processed_label: format_meter_tokens(total_processed),
            cache_hit_label,
            effective_model: None,
            last_compaction_seq: None,
            provisional: false,
        };
    };

    let unknown = context.estimating || context.context_limit == 0;
    let percent =
        (!unknown).then(|| context.current_tokens as f64 / context.context_limit as f64 * 100.0);
    let limit_prefix = if context.context_limit_is_estimate {
        "~"
    } else {
        ""
    };
    let current_label = if unknown {
        "Unavailable".into()
    } else {
        format!(
            "{} / {limit_prefix}{}",
            format_meter_tokens(context.current_tokens),
            format_meter_tokens(context.context_limit)
        )
    };
    let detail_label = percent.map_or_else(
        || "Context usage details, current usage unavailable".into(),
        |percent| format!("Context usage details, {percent:.0}% used"),
    );
    ContextMeterViewModel {
        percent,
        bar_percent: percent.unwrap_or_default().clamp(0.0, 100.0),
        current_label,
        detail_label,
        total_processed_label: format_meter_tokens(total_processed),
        cache_hit_label,
        effective_model: (!context.effective_model.is_empty())
            .then(|| context.effective_model.clone()),
        last_compaction_seq: context.last_compaction_seq,
        provisional: context.provisional,
    }
}

impl From<&threadlane_protocol::daemon::ContextWindowInfo> for ContextMeterContext {
    fn from(context: &threadlane_protocol::daemon::ContextWindowInfo) -> Self {
        Self {
            current_tokens: context.current_tokens,
            context_limit: context.context_limit,
            context_limit_is_estimate: context.context_limit_is_estimate,
            effective_model: context.effective_model.clone(),
            last_compaction_seq: context.last_compaction_seq,
            provisional: context.provisional,
            estimating: context.estimating,
        }
    }
}

/// Controlled context disclosure. Hosts own open state; usage policy stays shared.
pub fn context_meter_popover(
    meter: ContextMeterViewModel,
    open: bool,
    on_open_change: impl Fn(&bool, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Popover {
    let theme = cx.theme().colors;
    let displayed_percent = meter.percent.unwrap_or_default();
    let meter_color = if meter.percent.is_none() || displayed_percent == 0.0 {
        theme.muted_foreground
    } else if displayed_percent >= CONTEXT_METER_DANGER_PCT {
        theme.danger
    } else if displayed_percent >= CONTEXT_METER_WARN_PCT {
        theme.warning
    } else {
        theme.primary
    };
    Popover::new("context-window-popover")
        .anchor(Anchor::BottomRight)
        .appearance(false)
        .open(open)
        .on_open_change(on_open_change)
        .trigger(
            Button::new("context-meter-badge")
                .debug_selector(|| "context-meter-badge".into())
                .ghost()
                .selected(open)
                .rounded_full()
                .size_8()
                .p_0()
                .text_color(theme.muted_foreground)
                .accessibility_label(meter.detail_label.clone())
                .tooltip(meter.detail_label.clone())
                .when_some(meter.percent, |button, percent| {
                    button.child(
                        ProgressCircle::new("context-meter-circle")
                            .relative()
                            .flex_none()
                            .value(percent.clamp(0.0, 100.0) as f32)
                            .color(meter_color)
                            .size_6(),
                    )
                })
                .when(meter.percent.is_none(), |button| button.child("—")),
        )
        .content(move |_state, _window, _cx| {
            let current_summary = match meter.percent {
                Some(percent) => format!(
                    "{percent:.0}% · {}{}",
                    meter.current_label,
                    if meter.provisional {
                        " · provisional"
                    } else {
                        ""
                    }
                ),
                None => meter.current_label.clone(),
            };
            div()
                .debug_selector(|| "context-meter-details".into())
                .w(rems(21.25))
                .p_4()
                .rounded_xl()
                .border_1()
                .border_color(theme.border.opacity(0.8))
                .bg(theme.popover)
                .shadow_lg()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child("Current context"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(current_summary),
                        ),
                )
                .when(meter.percent.is_some(), |card| card.child(
                    div()
                        .w_full()
                        .h(rems(0.3125))
                        .rounded_full()
                        .bg(theme.muted.opacity(0.8))
                        .child(
                            div()
                                .h_full()
                                .w(relative((meter.bar_percent / 100.0) as f32))
                                .rounded_full()
                                .bg(meter_color),
                        ),
                ))
                .when_some(meter.effective_model.clone(), |card, effective_model| {
                    card.child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Model")
                            .child(effective_model),
                    )
                })
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Total processed")
                        .child(meter.total_processed_label.clone()),
                )
                .when_some(meter.cache_hit_label.clone(), |card, cache_hit| {
                    card.child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Cache hit")
                            .child(cache_hit),
                    )
                })
                .when_some(meter.last_compaction_seq, |card, sequence| {
                    card.child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Last compacted")
                            .child(format!("Record #{sequence}")),
                    )
                })
                .child(
                    div()
                        .pt_2()
                        .border_t_1()
                        .border_color(theme.border.opacity(0.4))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Context is compacted automatically when needed. Percent reflects the current model request."),
                )
        })
}
