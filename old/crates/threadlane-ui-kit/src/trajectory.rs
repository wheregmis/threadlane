//! Controlled trajectory atoms. Hosts retain the live projection, filters and selection.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Selectable};
use std::collections::HashSet;
use threadlane_protocol::daemon::TrajectoryEntry;

/// Grouping headers for the canonical trajectory event stream.
#[derive(Clone, Copy)]
pub enum TrajectorySection {
    Request(u32),
    Setup,
    Turn(u32),
}

pub fn trajectory_section_header(section: TrajectorySection, cx: &App) -> Div {
    let theme = cx.theme().colors;
    let (height, label) = match section {
        TrajectorySection::Request(request) => (rems(1.75), format!("Request #{request}")),
        TrajectorySection::Setup => (rems(1.25), "Setup".into()),
        TrajectorySection::Turn(turn) => (rems(1.375), format!("Turn {turn}")),
    };
    div()
        .h(height)
        .px_3()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme.border.opacity(0.5))
        .text_xs()
        .text_color(theme.muted_foreground)
        .when(matches!(section, TrajectorySection::Request(_)), |el| {
            el.bg(theme.muted.opacity(0.35))
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.accent)
        })
        .when(matches!(section, TrajectorySection::Setup), |el| {
            el.font_weight(FontWeight::MEDIUM)
        })
        .child(label)
}

/// Stable event identity within a host. Legacy projections without a journal
/// sequence retain their position fallback until the source supplies an identity.
pub fn trajectory_event_id(scope: &str, entry: &TrajectoryEntry, fallback: usize) -> SharedString {
    format!(
        "{scope}:{}:{}:{}:{}:{}",
        entry.run_id.as_deref().unwrap_or(""),
        entry.lane.as_deref().unwrap_or(""),
        entry.category,
        entry.correlation_id.as_deref().unwrap_or(""),
        entry
            .seq
            .map_or_else(|| format!("row-{fallback}"), |seq| seq.to_string())
    )
    .into()
}

/// Inspect one projected event. The caller supplies stable identity and selection.
pub fn trajectory_event_row(
    id: impl Into<ElementId>,
    entry: &TrajectoryEntry,
    preview: SharedString,
    selected: bool,
    on_inspect: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let id = id.into();
    let theme = cx.theme().colors;
    let (badge_bg, badge_fg, badge_label): (Hsla, Hsla, SharedString) =
        match entry.category.as_str() {
            "Tool" | "Tool runtime" => (theme.warning.opacity(0.18), theme.warning, "TOOL".into()),
            "Provider" => (
                theme.primary.opacity(0.18),
                theme.primary,
                "PROVIDER".into(),
            ),
            "Context Manifest" | "Manifest" => (
                theme.accent.opacity(0.14),
                theme.muted_foreground,
                "MANIFEST".into(),
            ),
            "Request" => (theme.primary.opacity(0.16), theme.accent, "REQUEST".into()),
            "Anomaly" => (theme.warning.opacity(0.20), theme.warning, "ANOMALY".into()),
            "Error" => (theme.danger.opacity(0.20), theme.danger, "ERROR".into()),
            "Input" => (theme.muted.opacity(0.8), theme.foreground, "INPUT".into()),
            "Assistant" => (
                theme.muted.opacity(0.8),
                theme.foreground,
                "ASSISTANT".into(),
            ),
            "Permission" => (
                theme.warning.opacity(0.18),
                theme.warning,
                "PERMISSION".into(),
            ),
            "Subagent" => (
                theme.primary.opacity(0.16),
                theme.primary,
                "SUBAGENT".into(),
            ),
            // Fusion routing transitions (armed, delegated,
            // escalated, compaction-switched) get their own
            // badge so mode activity stands out from tool noise.
            "Router" => (theme.accent.opacity(0.16), theme.accent, "ROUTER".into()),
            _ => (
                theme.muted.opacity(0.5),
                theme.muted_foreground,
                entry.category.clone().into(),
            ),
        };
    let dot_color = if entry.diagnostics.is_anomaly || entry.category == "Anomaly" {
        theme.warning
    } else if entry.category == "Error"
        || entry.detail.contains("Failed")
        || entry.detail.contains("Error")
        || matches!(
            entry.diagnostics.status.as_deref(),
            Some("Failed" | "failed")
        )
    {
        theme.danger
    } else if entry.category == "Tool" || entry.category == "Tool runtime" {
        theme.warning
    } else if entry.category == "Request" {
        theme.primary
    } else if entry.category == "Router" {
        theme.accent
    } else {
        theme.muted_foreground
    };
    let seq = entry.seq;
    let exit_code = entry.diagnostics.exit_code;
    let duration_ms = entry.diagnostics.duration_ms;
    let lane = entry.lane.clone();
    let description = match &lane {
        Some(lane) => format!("{lane} · {}", entry.summary),
        None => entry.summary.clone(),
    };
    Button::new(id.clone())
        .debug_selector(|| "trajectory-event-row".into())
        .accessibility_label(format!("Inspect {badge_label}: {description}"))
        .ghost()
        .selected(selected)
        .h_auto()
        .w_full()
        .p_0()
        .on_click(move |_, window, cx| on_inspect(window, cx))
        .child(
            div()
                .id((id, "content"))
                .debug_selector(|| "trajectory-event-content".into())
                .tooltip({
                    let tip = description;
                    move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    }
                })
                .h(rems(2.125))
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .border_b_1()
                .border_color(theme.border.opacity(0.45))
                .border_l_2()
                .border_color(if selected {
                    theme.accent
                } else {
                    theme.border.opacity(0.0)
                })
                .when(selected, |this| this.bg(theme.accent.opacity(0.16)))
                .child(
                    div()
                        .size(rems(0.375))
                        .flex_none()
                        .rounded_full()
                        .bg(dot_color),
                )
                .child(
                    div()
                        .w(rems(5.25))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .bg(badge_bg)
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(badge_fg)
                        .child(badge_label),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_sm()
                        .truncate()
                        .child(preview.clone()),
                )
                .children(exit_code.map(|code| {
                    let is_ok = code == 0;
                    div()
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .bg(if is_ok {
                            theme.success.opacity(0.15)
                        } else {
                            theme.danger.opacity(0.15)
                        })
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if is_ok { theme.success } else { theme.danger })
                        .child(format!("exit {code}"))
                }))
                .children(duration_ms.map(|duration| {
                    div()
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .bg(theme.muted.opacity(0.8))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if duration < 1000 {
                            format!("{duration}ms")
                        } else {
                            format!("{:.1}s", duration as f64 / 1000.0)
                        })
                }))
                .children(lane.map(|lane| {
                    div()
                        .max_w(rems(6.875))
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(lane)
                }))
                .children(seq.map(|seq| {
                    div()
                        .w(rems(3.25))
                        .text_right()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("#{seq}"))
                }))
                .into_any_element(),
        )
        .into_any_element()
}

#[derive(Default)]
pub struct TrajectorySummary {
    overview_positions: [HashSet<usize>; 3],
    overview_prefix: [Vec<u32>; 3],
    tool_count: usize,
    total_duration_ms: u64,
    anomaly_count: usize,
    max_turn: u32,
}

impl TrajectorySummary {
    pub fn from_entries(entries: &[TrajectoryEntry]) -> Self {
        let mut summary = Self::default();
        summary.append(entries);
        summary
    }
    pub fn append(&mut self, entries: &[TrajectoryEntry]) {
        let summary = self;
        for prefix in &mut summary.overview_prefix {
            if prefix.is_empty() {
                prefix.push(0);
            }
        }
        for entry in entries {
            let groups = [
                matches!(
                    entry.category.as_str(),
                    "Input" | "Context" | "Context Manifest" | "Queue" | "Request"
                ),
                matches!(
                    entry.category.as_str(),
                    "Operation" | "Step" | "Retry" | "Turn" | "Error" | "Provider" | "Anomaly"
                ),
                matches!(entry.category.as_str(), "Tool" | "Tool runtime"),
            ];
            for (prefix, present) in summary.overview_prefix.iter_mut().zip(groups) {
                prefix.push(prefix.last().copied().unwrap_or_default() + u32::from(present));
            }
            summary.tool_count += usize::from(groups[2]);
            summary.total_duration_ms = summary
                .total_duration_ms
                .saturating_add(entry.diagnostics.duration_ms.unwrap_or_default());
            summary.anomaly_count +=
                usize::from(entry.diagnostics.is_anomaly || entry.category == "Anomaly");
            summary.max_turn = summary.max_turn.max(entry.turn.unwrap_or_default());
        }
        let entry_count = summary.overview_prefix[0].len().saturating_sub(1);
        for (positions, prefix) in summary
            .overview_positions
            .iter_mut()
            .zip(&summary.overview_prefix)
        {
            positions.clear();
            for position in 0..48 {
                let start = (position * entry_count).div_ceil(48);
                let end = ((position + 1) * entry_count).div_ceil(48);
                if prefix[end] > prefix[start] {
                    positions.insert(position);
                }
            }
        }
    }
}

/// Empty feedback stays inside the event area so filters remain reachable.
pub fn trajectory_empty_state(filtered: bool, cx: &App) -> Div {
    div()
        .debug_selector(|| "trajectory-empty-state".into())
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .p_3()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(if filtered {
            "No matching trajectory events"
        } else {
            "No trajectory events yet"
        })
        .child(div().text_xs().child(if filtered {
            "Clear search or filters to see turns, tools, and results."
        } else {
            "Run the session to see turns, tools, and results."
        }))
}

/// Readable output of the selected projected event.
pub fn trajectory_entry_preview(entry: &TrajectoryEntry) -> AnyElement {
    use gpui_component::text::TextView;
    div()
        .debug_selector(|| "trajectory-entry-preview".into())
        .flex()
        .flex_col()
        .gap_3()
        .child(if entry.detail.is_empty() {
            div()
                .text_sm()
                .child("No preview content is available for this event.")
                .into_any_element()
        } else {
            TextView::markdown(
                format!("trajectory-preview-{}", entry.seq.unwrap_or(0)),
                entry.detail.clone(),
            )
            .selectable(true)
            .into_any_element()
        })
        .into_any_element()
}

/// Input, model and tool distribution over the event stream.
pub fn trajectory_overview(summary: &TrajectorySummary, cx: &App) -> Div {
    let theme = cx.theme().colors;
    let positions = &summary.overview_positions;
    let overview_lane = |label: &'static str, markers: &HashSet<usize>, color: Hsla| {
        div()
            .h(rems(1.125))
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w_12()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .h_3()
                    .flex()
                    .items_end()
                    .gap(rems(0.125))
                    .children((0..48).map(|index| {
                        div()
                            .flex_1()
                            .h(if markers.contains(&index) {
                                px(10.0)
                            } else {
                                px(2.0)
                            })
                            .rounded_sm()
                            .bg(if markers.contains(&index) {
                                color
                            } else {
                                theme.border.opacity(0.35)
                            })
                    })),
            )
    };
    div()
        .h(rems(3.625))
        .flex_none()
        .flex()
        .flex_col()
        .px_3()
        .py_1()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(overview_lane("Input", &positions[0], theme.success))
        .child(overview_lane("Model", &positions[1], theme.primary))
        .child(overview_lane("Tools", &positions[2], theme.warning))
}

pub fn trajectory_stats(summary: &TrajectorySummary, cx: &App) -> Div {
    let TrajectorySummary {
        max_turn,
        tool_count,
        total_duration_ms: total_dur_ms,
        anomaly_count,
        ..
    } = *summary;
    let theme = cx.theme().colors;
    let dur_label = if total_dur_ms < 1000 {
        format!("{total_dur_ms}ms total")
    } else {
        format!("{:.2}s total", total_dur_ms as f64 / 1000.0)
    };

    div()
        .h(rems(1.625))
        .flex_none()
        .flex()
        .items_center()
        .gap_4()
        .px_3()
        .border_b_1()
        .border_color(theme.border.opacity(0.4))
        .bg(theme.muted.opacity(0.15))
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(format!("{max_turn}")),
                )
                .child(plural_noun(max_turn as u64, "turn", "turns")),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(format!("{tool_count}")),
                )
                .child(plural_noun(tool_count as u64, "tool call", "tool calls")),
        )
        .child(
            div().flex().items_center().gap_1().child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(dur_label),
            ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .size(rems(0.375))
                        .rounded_full()
                        .bg(if anomaly_count > 0 {
                            theme.warning
                        } else {
                            theme.success
                        }),
                )
                .child(format!(
                    "{anomaly_count} {}",
                    plural_noun(anomaly_count as u64, "anomaly", "anomalies")
                )),
        )
}

fn plural_noun(count: u64, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

#[cfg(test)]
mod tests {
    fn trajectory_entry(
        category: &str,
        request: Option<u32>,
        turn: Option<u32>,
    ) -> threadlane_protocol::daemon::TrajectoryEntry {
        threadlane_protocol::daemon::TrajectoryEntry {
            seq: None,
            run_id: None,
            turn,
            request,
            category: category.into(),
            summary: category.into(),
            detail: String::new(),
            lane: None,
            correlation_id: None,
            diagnostics: Default::default(),
        }
    }
    #[test]
    fn trajectory_summary_is_computed_once_from_canonical_entries() {
        let mut tool = trajectory_entry("Tool", Some(1), Some(3));
        tool.diagnostics.duration_ms = Some(25);
        let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(4));
        anomaly.diagnostics.duration_ms = Some(75);
        anomaly.diagnostics.is_anomaly = true;

        let summary = super::TrajectorySummary::from_entries(&[tool, anomaly]);

        assert_eq!(summary.tool_count, 1);
        assert_eq!(summary.total_duration_ms, 100);
        assert_eq!(summary.anomaly_count, 1);
        assert_eq!(summary.max_turn, 4);
    }

    #[test]
    fn trajectory_summary_append_matches_full_rebuild() {
        let initial = vec![trajectory_entry("Input", Some(1), Some(1))];
        let mut tool = trajectory_entry("Tool", Some(1), Some(1));
        tool.diagnostics.duration_ms = Some(25);
        let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(2));
        anomaly.diagnostics.duration_ms = Some(75);
        let appended = vec![tool, anomaly];
        let mut incremental = super::TrajectorySummary::from_entries(&initial);
        incremental.append(&appended);

        let rebuilt = super::TrajectorySummary::from_entries(
            &initial.into_iter().chain(appended).collect::<Vec<_>>(),
        );
        assert_eq!(incremental.overview_positions, rebuilt.overview_positions);
        assert_eq!(incremental.tool_count, rebuilt.tool_count);
        assert_eq!(incremental.total_duration_ms, 100);
        assert_eq!(incremental.anomaly_count, rebuilt.anomaly_count);
        assert_eq!(incremental.max_turn, rebuilt.max_turn);
    }

    #[test]
    fn stats_nouns_use_singular_for_one() {
        assert_eq!(super::plural_noun(0, "turn", "turns"), "turns");
        assert_eq!(super::plural_noun(1, "turn", "turns"), "turn");
        assert_eq!(
            super::plural_noun(2, "tool call", "tool calls"),
            "tool calls"
        );
        assert_eq!(super::plural_noun(1, "anomaly", "anomalies"), "anomaly");
    }

    #[gpui::test]
    fn trajectory_row_selection_and_preview_fit_both_widths(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, IntoElement as _, ParentElement as _, Styled as _};
        struct Host {
            width: f32,
            selected: bool,
        }
        impl gpui::Render for Host {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                let mut entry = trajectory_entry("Tool", Some(1), Some(1));
                entry.seq = Some(42);
                entry.summary =
                    "Read a long path while preserving status and sequence columns".into();
                entry.detail = "Verified source output".into();
                entry.lane = Some("main".into());
                entry.diagnostics.exit_code = Some(0);
                entry.diagnostics.duration_ms = Some(1432);
                let owner = cx.entity().downgrade();
                gpui::div()
                    .w(gpui::px(self.width))
                    .flex()
                    .flex_col()
                    .child(super::trajectory_event_row(
                        "shared-event",
                        &entry,
                        entry.summary.clone().into(),
                        self.selected,
                        move |_, cx| {
                            let _ = owner.update(cx, |host, cx| {
                                host.selected = true;
                                cx.notify();
                            });
                        },
                        cx,
                    ))
                    .children(
                        self.selected
                            .then(|| super::trajectory_entry_preview(&entry)),
                    )
            }
        }
        cx.update(gpui_component::init);
        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let host = cx.new(|_| Host {
                width: 448.,
                selected: false,
            });
            *capture.borrow_mut() = Some(host.clone());
            gpui_component::Root::new(host, window, cx)
        });
        let host = captured.borrow_mut().take().unwrap();
        for width in [448., 960.] {
            host.update(cx, |host, cx| {
                host.width = width;
                host.selected = false;
                cx.notify();
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            let row = cx.debug_bounds("trajectory-event-row").unwrap();
            let content = cx.debug_bounds("trajectory-event-content").unwrap();
            assert!(content.left() >= row.left() && content.right() <= row.right());
            assert!(row.right() <= gpui::px(width));
            assert!(cx.debug_bounds("trajectory-entry-preview").is_none());
            cx.simulate_click(row.center(), gpui::Modifiers::default());
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            host.read_with(cx, |host, _| assert!(host.selected));
            assert!(cx.debug_bounds("trajectory-entry-preview").is_some());
        }
    }
}
