use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::TextView;
use gpui_component::theme::ActiveTheme;
use gpui_component::{IconName, Selectable, Sizable, WindowExt};

use std::borrow::Cow;
use threadlane_protocol::daemon::TrajectoryEntry;

use super::trajectory_cache::*;

/// Hosts supply projections without putting live services in the UI kit.
/// Desktop reads its retained AppState; WASM reads an immutable imported snapshot.
pub trait TrajectorySource: 'static {
    fn session_key(&self, cx: &App) -> Option<(PathBuf, String)>;
    /// Return (revision, replacement epoch). Increment revision on append;
    /// increment epoch whenever existing execution rows change or disappear.
    fn revision(&self, mode: TrajectoryMode, cx: &App) -> (u64, u64);
    fn entries<'a>(&'a self, mode: TrajectoryMode, cx: &'a App) -> Cow<'a, [TrajectoryEntry]>;
}

/// Shared virtual event stream, filters and inspector. Only presentation state
/// lives here; the source supplies canonical read-only data for each mode.
pub struct TrajectoryView<S: TrajectorySource> {
    source: S,
    trajectory_list_state: ListState,
    pub(crate) split_inspector: bool,
    pub(crate) trajectory_mode: TrajectoryMode,
    trajectory_search: String,
    trajectory_search_input: Entity<InputState>,
    trajectory_category: Option<String>,
    trajectory_lane: Option<String>,
    pub(crate) selected_trajectory_index: Option<usize>,
    pub(crate) trajectory_inspector_tab: TrajectoryInspectorTab,
    pub(crate) trajectory_cache: Option<TrajectoryRenderCache>,
    trajectory_raw_json: Option<(u64, usize, String)>,
    last_session_key: Option<(PathBuf, String)>,
    _subscriptions: Vec<Subscription>,
}

impl<S: TrajectorySource> TrajectoryView<S> {
    pub fn new(source: S, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let trajectory_list_state = ListState::new(0, ListAlignment::Top, window.rem_size() * 25.0);
        let trajectory_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search…"));
        let sub = cx.observe(&trajectory_search_input, |this, input, cx| {
            this.trajectory_search = input.read(cx).value().to_lowercase();
            cx.notify();
        });
        Self {
            source,
            trajectory_list_state,
            split_inspector: false,
            trajectory_mode: TrajectoryMode::Execution,
            trajectory_search: String::new(),
            trajectory_search_input,
            trajectory_category: None,
            trajectory_lane: None,
            selected_trajectory_index: None,
            trajectory_inspector_tab: TrajectoryInspectorTab::Overview,
            trajectory_cache: None,
            trajectory_raw_json: None,
            last_session_key: None,
            _subscriptions: vec![sub],
        }
    }

    fn render_trajectory_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self
            .trajectory_cache
            .as_ref()
            .and_then(|cache| cache.rows.get(index))
            .cloned()
        else {
            return Empty.into_any_element();
        };
        match row {
            TrajectoryRow::RequestHeader(request) => {
                crate::trajectory_section_header(crate::TrajectorySection::Request(request), cx)
                    .into_any_element()
            }
            TrajectoryRow::Setup => {
                crate::trajectory_section_header(crate::TrajectorySection::Setup, cx)
                    .into_any_element()
            }
            TrajectoryRow::TurnHeader(turn) => {
                crate::trajectory_section_header(crate::TrajectorySection::Turn(turn), cx)
                    .into_any_element()
            }
            TrajectoryRow::Entry(all_index) => {
                // Stale indices (cache rebuilt mid-render) render nothing
                // instead of panicking the paint.
                let Some(cache) = self.trajectory_cache.as_ref() else {
                    return Empty.into_any_element();
                };
                let Some(entry) = cache.all_entries.get(all_index) else {
                    return Empty.into_any_element();
                };
                let selected = Some(all_index) == self.selected_trajectory_index;
                let preview = cache.previews.get(all_index).cloned().unwrap_or_default();
                let view = cx.entity().clone();
                crate::trajectory_event_row(
                    crate::trajectory_event_id("trajectory", entry, all_index),
                    entry,
                    preview,
                    selected,
                    move |_, cx| {
                        view.update(cx, |this, cx| {
                            this.selected_trajectory_index = Some(all_index);
                            this.trajectory_inspector_tab = TrajectoryInspectorTab::Overview;
                            cx.notify();
                        });
                    },
                    cx,
                )
            }
        }
    }

    fn render_trajectory(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (revision, epoch) = self.source.revision(self.trajectory_mode, cx);
        let key = TrajectoryCacheKey {
            revision,
            epoch,
            mode: self.trajectory_mode,
            query: self.trajectory_search.clone(),
            category: self.trajectory_category.clone(),
            lane: self.trajectory_lane.clone(),
        };
        if self
            .trajectory_cache
            .as_ref()
            .is_none_or(|cache| cache.key != key)
        {
            let selection_can_stay = matches!(
                key.mode,
                TrajectoryMode::Execution | TrajectoryMode::Requests
            ) && self
                .trajectory_cache
                .as_ref()
                .is_some_and(|cache| cache.key.mode == key.mode && cache.key.epoch == key.epoch);
            let selected_entry = if selection_can_stay {
                None
            } else {
                self.trajectory_cache
                    .as_ref()
                    .filter(|cache| cache.key.mode == key.mode)
                    .and_then(|cache| {
                        self.selected_trajectory_index.and_then(|index| {
                            cache
                                .all_entries
                                .get(index)
                                .map(|entry| (index, entry.clone()))
                        })
                    })
            };
            let cached_len = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.all_entries.len());
            let cached_filtered_len = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.filtered_indices.len());
            let projection_matches = self.trajectory_cache.as_ref().is_some_and(|cache| {
                cache.key.epoch == key.epoch
                    && cache.key.mode == key.mode
                    && cache.key.query == key.query
                    && cache.key.category == key.category
                    && cache.key.lane == key.lane
            });
            let mut cached_summary = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.summary))
                .unwrap_or_default();
            let mut cached_categories = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.categories))
                .unwrap_or_default();
            let mut cached_lane_latest = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.lane_latest))
                .unwrap_or_default();
            let mut cached_filtered_indices = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.filtered_indices))
                .unwrap_or_default();
            let mut cached_previews = self
                .trajectory_cache
                .as_mut()
                .map(|cache| std::mem::take(&mut cache.previews))
                .unwrap_or_default();
            let cached_entries = self
                .trajectory_cache
                .as_mut()
                .filter(|cache| cache.key.mode == key.mode)
                .map(|cache| std::mem::take(&mut cache.all_entries))
                .unwrap_or_default();
            let cached_epoch = self
                .trajectory_cache
                .as_ref()
                .map_or(epoch, |cache| cache.key.epoch);
            let source = self.source.entries(self.trajectory_mode, cx);
            let (all_entries, appended) = match self.trajectory_mode {
                TrajectoryMode::Execution | TrajectoryMode::Requests => {
                    reconcile_trajectory_entries_by_epoch(
                        cached_entries,
                        &source,
                        cached_epoch,
                        epoch,
                    )
                }
                _ => reconcile_trajectory_entries_with_append(cached_entries, &source),
            };
            // Replacement projections may reorder or remove entries. Keep the
            // inspector on the same journal event, never on its former index.
            if !selection_can_stay {
                self.selected_trajectory_index =
                    selected_entry.and_then(|(previous_index, selected)| {
                        all_entries.iter().enumerate().find_map(|(index, entry)| {
                            let same = if selected.seq.is_some()
                                || selected.correlation_id.is_some()
                            {
                                crate::trajectory_event_id("selection", &selected, previous_index)
                                    == crate::trajectory_event_id("selection", entry, index)
                            } else {
                                entry == &selected
                            };
                            same.then_some(index)
                        })
                    });
            }
            let (categories, lane_latest, filtered_indices) = if projection_matches && appended {
                extend_trajectory_facets(
                    Arc::make_mut(&mut cached_categories),
                    Arc::make_mut(&mut cached_lane_latest),
                    &mut cached_filtered_indices,
                    &all_entries,
                    cached_len,
                    &key,
                );
                (
                    cached_categories,
                    cached_lane_latest,
                    cached_filtered_indices,
                )
            } else {
                let mut categories = Vec::new();
                let mut lane_latest = std::collections::BTreeMap::new();
                let mut filtered_indices = Vec::new();
                extend_trajectory_facets(
                    &mut categories,
                    &mut lane_latest,
                    &mut filtered_indices,
                    &all_entries,
                    0,
                    &key,
                );
                (
                    Arc::new(categories),
                    Arc::new(lane_latest),
                    filtered_indices,
                )
            };
            let lanes = Arc::new(lane_latest.keys().cloned().collect());
            let previews = if projection_matches && appended {
                extend_trajectory_previews(&mut cached_previews, &all_entries, cached_len);
                cached_previews
            } else {
                let mut previews = Vec::with_capacity(all_entries.len());
                extend_trajectory_previews(&mut previews, &all_entries, 0);
                previews
            };
            let (rows, extends_previous) = if projection_matches && appended {
                let mut rows = self
                    .trajectory_cache
                    .as_mut()
                    .map(|cache| std::mem::take(&mut cache.rows))
                    .unwrap_or_default();
                extend_trajectory_rows(
                    &mut rows,
                    &all_entries,
                    &filtered_indices,
                    cached_filtered_len,
                    self.trajectory_mode,
                );
                (rows, true)
            } else {
                let rows =
                    build_trajectory_rows(&all_entries, &filtered_indices, self.trajectory_mode);
                let extends_previous = self
                    .trajectory_cache
                    .as_ref()
                    .is_some_and(|cache| rows.starts_with(&cache.rows));
                (rows, extends_previous)
            };
            let summary = if projection_matches && appended {
                cached_summary.append(&all_entries[cached_len..]);
                cached_summary
            } else {
                crate::TrajectorySummary::from_entries(&all_entries)
            };
            let previous_row_count = self
                .trajectory_cache
                .as_ref()
                .map_or(0, |cache| cache.rows.len());
            if extends_previous {
                self.trajectory_list_state.splice(
                    previous_row_count..previous_row_count,
                    rows.len() - previous_row_count,
                );
            } else {
                self.trajectory_list_state.reset(rows.len());
            }
            self.trajectory_raw_json = None;
            self.trajectory_cache = Some(TrajectoryRenderCache {
                key,
                all_entries,
                categories,
                lanes,
                lane_latest,
                filtered_indices,
                previews,
                rows,
                summary,
            });
        }
        let inspector_tab = self.trajectory_inspector_tab;
        let selected_index = self.selected_trajectory_index;
        if let Some(index) = (inspector_tab == TrajectoryInspectorTab::Raw)
            .then_some(selected_index)
            .flatten()
        {
            let needs_raw = self.trajectory_raw_json.as_ref().is_none_or(
                |(cached_revision, cached_index, _)| {
                    *cached_revision != revision || *cached_index != index
                },
            );
            if needs_raw {
                self.trajectory_raw_json = self
                    .trajectory_cache
                    .as_ref()
                    .and_then(|cache| cache.all_entries.get(index))
                    .map(|entry| (revision, index, format_trajectory_raw_json(entry)));
            }
        }
        let Some(cache) = self.trajectory_cache.as_ref() else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().colors.muted_foreground)
                .child("Trajectory unavailable.")
                .into_any_element();
        };
        let all_entries = &cache.all_entries;
        let categories = Arc::clone(&cache.categories);
        let lanes = Arc::clone(&cache.lanes);
        let lane_latest = Arc::clone(&cache.lane_latest);
        let entries = &cache.filtered_indices;
        let theme = cx.theme().colors;
        let selected_entry = selected_index
            .and_then(|index| all_entries.get(index))
            .cloned();
        let selected_raw_json = (inspector_tab == TrajectoryInspectorTab::Raw)
            .then(|| {
                self.trajectory_raw_json
                    .as_ref()
                    .map(|(_, _, raw)| raw.clone())
            })
            .flatten();
        let inspector = selected_entry.map(|entry| {
            let close_view = cx.entity().clone();
            let inspector_view = cx.entity().clone();
            let model_visible = entry.diagnostics.model_visible || matches!(
                entry.category.as_str(),
                "Input" | "Assistant" | "Context" | "Context Manifest" | "Tool"
            );
            let provenance = match entry.category.as_str() {
                "Input" => "User transcript · model-visible",
                "Assistant" => "Assistant transcript · model-visible",
                "Context" | "Context Manifest" => "Runtime context package · model-visible",
                "Tool" | "Tool runtime" => "Tool transcript · model-visible",
                "Anomaly" => "Automated diagnostic anomaly · durable",
                "Error" => "Runtime diagnostic · durable",
                _ => "Runtime lifecycle record · durable",
            };
            let mut metadata_items = vec![
                entry.seq.map(|value| ("Sequence", format!("#{value}"))),
                entry.request.map(|value| ("Request", format!("#{value}"))),
                entry.turn.map(|value| ("Turn", value.to_string())),
                entry.run_id.clone().map(|value| ("Run", value)),
                entry.lane.clone().map(|value| ("Lane", value)),
                entry.correlation_id.clone().map(|value| ("Call / Correlation", value)),
                entry.diagnostics.status.clone().map(|value| ("Status", value)),
                entry.diagnostics.duration_ms.map(|value| {
                    (
                        "Duration",
                        if value < 1000 {
                            format!("{value} ms")
                        } else {
                            format!("{:.2} s", value as f64 / 1000.0)
                        },
                    )
                }),
                entry.diagnostics.exit_code.map(|value| ("Exit Code", value.to_string())),
                entry.diagnostics.output_bytes.map(|value| ("Output Size", format!("{value} bytes"))),
                entry.diagnostics.token_estimate.map(|value| ("Est. Tokens", format!("~{value}"))),
                entry.diagnostics.items_count.map(|value| ("Item Count", value.to_string())),
            ];
            if !entry.diagnostics.files_mutated.is_empty() {
                metadata_items.push(Some(("Files Mutated", entry.diagnostics.files_mutated.join(", "))));
            }
            if !entry.diagnostics.commands_executed.is_empty() {
                metadata_items.push(Some(("Commands Executed", entry.diagnostics.commands_executed.join(", "))));
            }
            let metadata = metadata_items.into_iter().flatten();
            let (header_bg, header_fg, header_tag): (Hsla, Hsla, SharedString) = match entry.category.as_str() {
                "Tool" | "Tool runtime" => (theme.warning.opacity(0.18), theme.warning, "TOOL".into()),
                "Provider" => (theme.primary.opacity(0.18), theme.primary, "PROVIDER".into()),
                "Context Manifest" | "Manifest" => (theme.accent.opacity(0.14), theme.muted_foreground, "MANIFEST".into()),
                "Request" => (theme.primary.opacity(0.16), theme.accent, "REQUEST".into()),
                "Anomaly" => (theme.warning.opacity(0.20), theme.warning, "ANOMALY".into()),
                "Error" => (theme.danger.opacity(0.20), theme.danger, "ERROR".into()),
                _ => (theme.muted.opacity(0.5), theme.muted_foreground, entry.category.clone().into()),
            };
            div()
                .debug_selector(|| "trajectory-inspector".into())
                .w_full()
                .min_w_0()
                .when(self.split_inspector, |inspector| inspector.w(relative(0.55)).min_w(rems(20.0)))
                .h_full()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .child(
                    div()
                        .h_12()
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(header_bg)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(header_fg)
                                .child(header_tag),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(entry.summary.clone()),
                        )
                        .children(entry.diagnostics.duration_ms.map(|dur| {
                            let dur_str = if dur < 1000 { format!("{dur}ms") } else { format!("{:.1}s", dur as f64 / 1000.0) };
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme.muted.opacity(0.7))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(dur_str)
                        }))
                        .child(
                            Button::new("copy-trajectory-row")
                                .accessibility_label("Copy trajectory entry")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Copy)
                                .tooltip("Copy trajectory entry")
                                .on_click({
                                    let text = format!(
                                        "seq:{:?} turn:{:?} category:{} summary:{} detail:{} lane:{:?} run:{:?} call:{:?}",
                                        entry.seq, entry.turn, entry.category, entry.summary,
                                        entry.detail, entry.lane, entry.run_id, entry.correlation_id,
                                    );
                                    move |_, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                        window.push_notification(
                                            Notification::info("Copied trajectory entry"),
                                            cx,
                                        );
                                    }
                                }),
                        )
                        .child(
                            Button::new("close-trajectory-inspector")
                                .accessibility_label("Close trajectory inspector")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip("Close inspector")
                                .on_click(move |_, _, cx| {
                                    close_view.update(cx, |this, cx| {
                                        this.selected_trajectory_index = None;
                                        cx.notify();
                                    })
                                }),
                        ),
                )
                .child(
                    div()
                        .min_h(rems(2.375))
                        .px_3()
                        .py_1()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .children([
                            ("Overview", TrajectoryInspectorTab::Overview),
                            ("Preview", TrajectoryInspectorTab::Preview),
                            ("Raw", TrajectoryInspectorTab::Raw),
                            ("Source", TrajectoryInspectorTab::Source),
                        ]
                        .into_iter()
                        .map(|(label, tab)| {
                            let view = inspector_view.clone();
                            let tip = match tab {
                                TrajectoryInspectorTab::Overview => {
                                    "Overview of the selected entry"
                                }
                                TrajectoryInspectorTab::Preview => {
                                    "Preview of the selected entry"
                                }
                                TrajectoryInspectorTab::Raw => {
                                    "Raw JSON of the selected entry"
                                }
                                TrajectoryInspectorTab::Source => {
                                    "Source of the selected entry"
                                }
                            };
                            Button::new(SharedString::from(format!("trajectory-inspector-{label}")))
                                .debug_selector(move || {
                                    format!("trajectory-inspector-{label}")
                                })
                                .ghost()
                                .small()
                                .selected(inspector_tab == tab)
                                .label(label)
                                .accessibility_label(tip)
                                .tooltip(tip)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_inspector_tab = tab;
                                        cx.notify();
                                    })
                                })
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(match inspector_tab {
                            TrajectoryInspectorTab::Overview => div()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(
                                    div()
                                        .p_3()
                                        .rounded_lg()
                                        .bg(theme.muted.opacity(0.3))
                                        .border_1()
                                        .border_color(theme.border.opacity(0.5))
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .children(metadata.map(|(label, value)| {
                                            div()
                                                .flex()
                                                .gap_2()
                                                .text_sm()
                                                .child(
                                                    div()
                                                        .w(rems(6.875))
                                                        .flex_none()
                                                        .text_xs()
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(theme.muted_foreground)
                                                        .child(label),
                                                )
                                                .child(div().min_w_0().flex_1().text_xs().child(value.clone()))
                                        })),
                                )
                                .children(entry.diagnostics.raw.as_ref().map(|raw_args| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("INPUT ARGUMENTS"))
                                        .child(TextView::markdown(
                                            format!("trajectory-args-{}", entry.seq.unwrap_or(0)),
                                            format!("```json\n{}\n```", raw_args),
                                        ).selectable(true))
                                }))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("VISIBILITY"))
                                .child(div().text_sm().child(if model_visible { "Model-visible transcript/context" } else { "Runtime-only durable diagnostic" }))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("SUMMARY"))
                                .child(div().text_sm().child(entry.summary.clone()))
                                .into_any_element(),
                            TrajectoryInspectorTab::Preview => div().flex().flex_col().gap_3()
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("OUTPUT PREVIEW"))
                                .child(crate::trajectory_entry_preview(&entry)).into_any_element(),
                            TrajectoryInspectorTab::Raw => {
                                let raw_json = selected_raw_json.clone().unwrap_or_default();
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("CANONICAL PROJECTION (JSON)"))
                                    .child(TextView::markdown(
                                        format!("trajectory-raw-{}", entry.seq.unwrap_or(0)),
                                        format!("```json\n{raw_json}\n```"),
                                    ).selectable(true))
                                    .into_any_element()
                            }
                            TrajectoryInspectorTab::Source => div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("PROVENANCE"))
                                .child(div().text_sm().child(entry.diagnostics.source.clone().unwrap_or_else(|| provenance.to_string())))
                                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("LINEAGE"))
                                .child(div().text_sm().child(format!(
                                    "Request {} · Turn {} · Lane {}",
                                    entry.request.map_or("—".to_string(), |request| format!("#{request}")),
                                    entry.turn.map_or("—".to_string(), |turn| turn.to_string()),
                                    entry.lane.as_deref().unwrap_or("—"),
                                )))
                                .children(entry.diagnostics.parent_id.as_ref().map(|p| {
                                    div().flex().flex_col().gap_1()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("PARENT ENTRY"))
                                        .child(div().text_sm().font_family(cx.theme().mono_font_family.clone()).child(p.clone()))
                                }))
                                .children(entry.diagnostics.result_id.as_ref().map(|r| {
                                    div().flex().flex_col().gap_1()
                                        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("RESULT ENTRY"))
                                        .child(div().text_sm().font_family(cx.theme().mono_font_family.clone()).child(r.clone()))
                                }))
                                .children(entry.correlation_id.clone().map(|id| div().text_sm().child(format!("Correlation: {id}"))))
                                .into_any_element(),
                        }),
                )
        });
        let overview = crate::trajectory_overview(&cache.summary, cx);
        let category_label = self
            .trajectory_category
            .clone()
            .unwrap_or_else(|| "All events".into());
        let lane_label = self
            .trajectory_lane
            .clone()
            .unwrap_or_else(|| format!("{} lanes", lanes.len()));
        let category_view = cx.entity().clone();
        let lane_view = cx.entity().clone();
        let mode_view = cx.entity().clone();
        let mode_label = match self.trajectory_mode {
            TrajectoryMode::Execution => "Execution",
            TrajectoryMode::Requests => "Requests",
            TrajectoryMode::ModelContext => "Model Context",
            TrajectoryMode::DurableEvents => "Durable Events",
            TrajectoryMode::Recovery => "Recovery",
        };
        let toolbar = div()
            .min_h(rems(2.375))
            .py_1()
            .flex_wrap()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                Button::new("trajectory-mode-filter")
                    .debug_selector(|| "trajectory-mode-filter".into())
                    .ghost()
                    .small()
                    .label(mode_label)
                    .accessibility_label(format!("Trajectory mode filter: {mode_label}"))
                    .tooltip("Filter trajectory by mode")
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, window, cx| {
                        menu.focus_handle(cx).focus(window, cx);
                        let mut menu = menu;
                        for (label, mode) in [
                            ("Execution", TrajectoryMode::Execution),
                            ("Requests", TrajectoryMode::Requests),
                            ("Model Context", TrajectoryMode::ModelContext),
                            ("Durable Events", TrajectoryMode::DurableEvents),
                            ("Recovery", TrajectoryMode::Recovery),
                        ] {
                            let view = mode_view.clone();
                            menu =
                                menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_mode = mode;
                                        this.trajectory_category = None;
                                        this.trajectory_lane = None;
                                        this.selected_trajectory_index = None;
                                        cx.notify();
                                    });
                                }));
                        }
                        menu
                    }),
            )
            .child(
                Button::new("trajectory-category-filter")
                    .debug_selector(|| "trajectory-category-filter".into())
                    .ghost()
                    .small()
                    .label(category_label.clone())
                    .accessibility_label(format!("Trajectory category filter: {category_label}"))
                    .tooltip("Filter trajectory by category")
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, window, cx| {
                        menu.focus_handle(cx).focus(window, cx);
                        let all_view = category_view.clone();
                        let mut menu = menu.item(PopupMenuItem::new("All events").on_click(
                            move |_, _, cx| {
                                all_view.update(cx, |this, cx| {
                                    this.trajectory_category = None;
                                    cx.notify();
                                });
                            },
                        ));
                        for category in categories.iter().cloned() {
                            let selected = category.clone();
                            let view = category_view.clone();
                            menu = menu.item(PopupMenuItem::new(category).on_click(
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.trajectory_category = Some(selected.clone());
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .children((lanes.len() > 1).then(|| {
                Button::new("trajectory-lane-filter")
                    .debug_selector(|| "trajectory-lane-filter".into())
                    .ghost()
                    .small()
                    .label(lane_label.clone())
                    .accessibility_label(format!("Trajectory lane filter: {lane_label}"))
                    .tooltip("Filter trajectory by lane")
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, window, cx| {
                        menu.focus_handle(cx).focus(window, cx);
                        let all_view = lane_view.clone();
                        let mut menu =
                            menu.item(PopupMenuItem::new("All lanes").on_click(move |_, _, cx| {
                                all_view.update(cx, |this, cx| {
                                    this.trajectory_lane = None;
                                    cx.notify();
                                });
                            }));
                        for lane in lanes.iter().cloned() {
                            let selected = lane.clone();
                            let view = lane_view.clone();
                            let latest = lane_latest.get(&lane).cloned().unwrap_or_default();
                            menu = menu.item(
                                PopupMenuItem::new(format!("{lane} — {latest}")).on_click(
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.trajectory_lane = Some(selected.clone());
                                            cx.notify();
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    })
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(rems(8.0))
                    .max_w(rems(17.5))
                    .h_8()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        Input::new(&self.trajectory_search_input)
                            .appearance(false)
                            .aria_label("Search trajectory"),
                    ),
            );
        let stats_bar = crate::trajectory_stats(&cache.summary, cx);
        // With no events in this mode, the blank meter and all-zero stats say nothing. The
        // toolbar stays: another mode or a filter reset may be the way back to events.
        let has_events = !cache.all_entries.is_empty();

        let layout_view = cx.entity().downgrade();
        div()
            .id("session-trajectory")
            .relative()
            .min_w_0()
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(has_events, |panel| {
                panel.child(overview.debug_selector(|| "trajectory-overview".into()))
            })
            .child(toolbar)
            .when(has_events, |panel| {
                panel.child(stats_bar.debug_selector(|| "trajectory-stats".into()))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(self.split_inspector || selected_index.is_none(), |body| {
                        body.child(
                            div()
                                .id("trajectory-events-container")
                                .debug_selector(|| "trajectory-events-container".into())
                                .relative()
                                .flex_1()
                                .min_w_0()
                                .min_h_0()
                                .child(if entries.is_empty() {
                                    crate::trajectory_empty_state(
                                        !self.trajectory_search.is_empty()
                                            || self.trajectory_category.is_some()
                                            || self.trajectory_lane.is_some(),
                                        cx,
                                    )
                                    .into_any_element()
                                } else {
                                    list(
                                        self.trajectory_list_state.clone(),
                                        cx.processor(Self::render_trajectory_row),
                                    )
                                    .size_full()
                                    .with_sizing_behavior(ListSizingBehavior::Auto)
                                    .into_any_element()
                                })
                                .child(div().absolute().inset_0().child(
                                    gpui_component::scroll::Scrollbar::vertical(
                                        &self.trajectory_list_state,
                                    ),
                                )),
                        )
                    })
                    .children(inspector),
            )
            .child(
                canvas(
                    move |bounds, window, cx| {
                        // Observe the actual panel, which may be narrower than its window.
                        let split = bounds.size.width >= window.rem_size() * 44.0;
                        let _ = layout_view.update(cx, |view, cx| {
                            if view.split_inspector != split {
                                view.split_inspector = split;
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .size_full(),
            )
            .into_any_element()
    }
}

impl<S: TrajectorySource> Render for TrajectoryView<S> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_key = self.source.session_key(cx);
        if session_key != self.last_session_key {
            self.last_session_key = session_key;
            self.trajectory_category = None;
            self.trajectory_lane = None;
            self.selected_trajectory_index = None;
            self.trajectory_search.clear();
            self.trajectory_cache = None;
            self.trajectory_raw_json = None;
            self.trajectory_search_input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }
        self.render_trajectory(cx)
    }
}
