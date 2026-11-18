use crate::context_meter::format_meter_tokens;
use gpui::{prelude::*, *};
use gpui_component::ActiveTheme;
use threadlane_protocol::efficiency::TokenEfficiencyReport;
pub fn token_efficiency(
    report: Option<&TokenEfficiencyReport>,
    is_generating: bool,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let mut section = div()
        .debug_selector(|| "environment-token-efficiency".into())
        .mt_1()
        .pt_2()
        .px_2()
        .border_t_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap_1()
        .text_xs()
        .child(
            div()
                .text_color(theme.muted_foreground)
                .child("Token efficiency"),
        );
    let Some(report) = report else {
        return section
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child("Available after a provider run"),
            )
            .into_any_element();
    };
    let requests: u64 = report
        .lanes
        .values()
        .map(|lane| lane.provider_requests)
        .sum();
    let failures: u64 = report
        .lanes
        .values()
        .map(|lane| lane.failed_provider_requests)
        .sum();
    let reductions: u64 = report
        .lanes
        .values()
        .map(|lane| lane.reduced_context_items)
        .sum();
    let child_tokens: u64 = report
        .lanes
        .iter()
        .filter(|(id, _)| id.as_str() != "main")
        .map(|(_, lane)| lane.usage.processed_tokens())
        .sum();
    if requests == 0 && report.usage.processed_tokens() == 0 {
        return section
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child("No provider usage reported"),
            )
            .into_any_element();
    }
    for (label, value, visible) in [
        (
            "Processed tokens",
            format_meter_tokens(report.usage.processed_tokens()),
            true,
        ),
        (
            "Uncached input",
            format_meter_tokens(report.usage.uncached_input_tokens),
            true,
        ),
        (
            "Cache reads / writes",
            format!(
                "{} / {}",
                format_meter_tokens(report.usage.cache_read_tokens),
                format_meter_tokens(report.usage.cache_write_tokens)
            ),
            report.usage.cache_read_tokens > 0 || report.usage.cache_write_tokens > 0,
        ),
        (
            "Output tokens",
            format_meter_tokens(report.usage.output_tokens),
            true,
        ),
        ("Child tokens", format_meter_tokens(child_tokens), child_tokens > 0),
        (
            "Tokens / completed run",
            report
                .session_tokens_per_completed_foreground_run
                .map(|tokens| format_meter_tokens(tokens.round() as u64))
                .unwrap_or_else(|| "—".into()),
            report.completed_foreground_runs > 1,
        ),
        ("Requests / failed", format!("{requests} / {failures}"), true),
        ("Reduced context items", reductions.to_string(), reductions > 0),
        (
            "Compactions / rereads",
            format!(
                "{} / {}",
                report.compactions, report.repeated_snapshot_reads
            ),
            report.compactions > 0 || report.repeated_snapshot_reads > 0,
        ),
    ] {
        if !visible {
            continue;
        }
        section = section.child(
            div()
                .debug_selector(move || format!("efficiency-{label}").into())
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_color(theme.muted_foreground).child(label))
                .child(div().flex_none().child(value)),
        );
    }
    section
        .child(
            div()
                .text_color(theme.muted_foreground)
                .child(if is_generating {
                    "All lanes · last journal snapshot. Updates after this run."
                } else {
                    "Reported usage · all lanes, cached tokens and failed attempts."
                }),
        )
        .into_any_element()
}
