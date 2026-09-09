use gpui::*;
use gpui_component::button::Toggle;
use gpui_component::Selectable;

use crate::state::SubagentActivityStatus;

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

impl ContextMeterContext {
    pub fn new(
        current_tokens: u64,
        context_limit: u64,
        context_limit_is_estimate: bool,
        effective_model: String,
        last_compaction_seq: Option<u64>,
        provisional: bool,
        estimating: bool,
    ) -> Self {
        Self {
            current_tokens,
            context_limit,
            context_limit_is_estimate,
            effective_model,
            last_compaction_seq,
            provisional,
            estimating,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ContextMeterMetrics {
    pub billed_input_tokens: u64,
    pub output_tokens: u64,
    pub cache_hit_percent: Option<u64>,
}

impl ContextMeterMetrics {
    pub fn new(
        billed_input_tokens: u64,
        output_tokens: u64,
        cache_hit_percent: Option<u64>,
    ) -> Self {
        Self {
            billed_input_tokens,
            output_tokens,
            cache_hit_percent,
        }
    }
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

#[derive(IntoElement)]
pub struct ContextMeterTrigger {
    pub toggle: Toggle,
    pub selected: bool,
}

impl ContextMeterTrigger {
    pub fn new(toggle: Toggle, selected: bool) -> Self {
        Self { toggle, selected }
    }
}

impl Selectable for ContextMeterTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for ContextMeterTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.toggle.checked(self.selected)
    }
}

#[derive(IntoElement)]
pub struct SubagentPopoverTrigger {
    pub toggle: Toggle,
    pub selected: bool,
}

impl SubagentPopoverTrigger {
    pub fn new(toggle: Toggle, selected: bool) -> Self {
        Self { toggle, selected }
    }
}

impl Selectable for SubagentPopoverTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for SubagentPopoverTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.toggle.checked(self.selected)
    }
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
            current_label: "Estimating…".into(),
            detail_label: "Context usage details, estimating usage".into(),
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
        "Estimating…".into()
    } else {
        format!(
            "{} / {limit_prefix}{}",
            format_meter_tokens(context.current_tokens),
            format_meter_tokens(context.context_limit)
        )
    };
    let detail_label = percent.map_or_else(
        || "Context usage details, estimating usage".into(),
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
