use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use gpui::SharedString;

use crate::state::TrajectoryEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrajectoryMode {
    #[default]
    Execution,
    Requests,
    ModelContext,
    DurableEvents,
    Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrajectoryInspectorTab {
    #[default]
    Overview,
    Preview,
    Raw,
    Source,
}

pub fn format_trajectory_raw_json(entry: &TrajectoryEntry) -> String {
    serde_json::to_string_pretty(entry).unwrap_or_else(|_| entry.detail.clone())
}

#[cfg(test)]
pub fn reconcile_trajectory_entries(
    cached: Vec<TrajectoryEntry>,
    source: &[TrajectoryEntry],
) -> Vec<TrajectoryEntry> {
    reconcile_trajectory_entries_with_append(cached, source).0
}

pub fn reconcile_trajectory_entries_with_append(
    mut cached: Vec<TrajectoryEntry>,
    source: &[TrajectoryEntry],
) -> (Vec<TrajectoryEntry>, bool) {
    if source.starts_with(&cached) {
        cached.extend_from_slice(&source[cached.len()..]);
        (cached, true)
    } else {
        (source.to_vec(), false)
    }
}

pub fn reconcile_trajectory_entries_by_epoch(
    mut cached: Vec<TrajectoryEntry>,
    source: &[TrajectoryEntry],
    cached_epoch: u64,
    source_epoch: u64,
) -> (Vec<TrajectoryEntry>, bool) {
    if cached_epoch == source_epoch && source.len() >= cached.len() {
        cached.extend_from_slice(&source[cached.len()..]);
        (cached, true)
    } else {
        reconcile_trajectory_entries_with_append(cached, source)
    }
}

pub fn contains_case_insensitive(haystack: &str, lowercase_query: &str) -> bool {
    if lowercase_query.is_empty() {
        return true;
    }
    if lowercase_query.is_ascii() && haystack.is_ascii() {
        return haystack
            .as_bytes()
            .windows(lowercase_query.len())
            .any(|window| window.eq_ignore_ascii_case(lowercase_query.as_bytes()));
    }
    haystack.to_lowercase().contains(lowercase_query)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrajectoryCacheKey {
    pub revision: u64,
    pub epoch: u64,
    pub mode: TrajectoryMode,
    pub query: String,
    pub category: Option<String>,
    pub lane: Option<String>,
}

pub fn extend_trajectory_facets(
    categories: &mut Vec<String>,
    lane_latest: &mut BTreeMap<String, String>,
    filtered_indices: &mut Vec<usize>,
    entries: &[TrajectoryEntry],
    start: usize,
    key: &TrajectoryCacheKey,
) {
    for (index, entry) in entries.iter().enumerate().skip(start) {
        if let Err(position) = categories.binary_search(&entry.category) {
            categories.insert(position, entry.category.clone());
        }
        if let Some(lane) = &entry.lane {
            lane_latest.insert(lane.clone(), entry.summary.clone());
        }
        let matches = key
            .category
            .as_ref()
            .is_none_or(|category| &entry.category == category)
            && key
                .lane
                .as_ref()
                .is_none_or(|lane| entry.lane.as_ref() == Some(lane))
            && [
                entry.category.as_str(),
                entry.summary.as_str(),
                entry.detail.as_str(),
                entry.lane.as_deref().unwrap_or(""),
                entry.correlation_id.as_deref().unwrap_or(""),
            ]
            .iter()
            .any(|value| contains_case_insensitive(value, &key.query));
        if matches {
            filtered_indices.push(index);
        }
    }
}

pub fn extend_trajectory_previews(
    previews: &mut Vec<SharedString>,
    entries: &[TrajectoryEntry],
    start: usize,
) {
    previews.reserve(entries.len().saturating_sub(start));
    previews.extend(entries[start..].iter().map(|entry| {
        if entry.detail.trim().is_empty() {
            entry.summary.clone().into()
        } else {
            format!("{}  {}", entry.summary, entry.detail.replace('\n', " ")).into()
        }
    }));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrajectoryRow {
    RequestHeader(u32),
    Setup,
    TurnHeader(u32),
    Entry(usize),
}

pub fn build_trajectory_rows(
    all_entries: &[TrajectoryEntry],
    filtered_indices: &[usize],
    mode: TrajectoryMode,
) -> Vec<TrajectoryRow> {
    let mut rows = Vec::with_capacity(filtered_indices.len());
    extend_trajectory_rows(&mut rows, all_entries, filtered_indices, 0, mode);
    rows
}

pub fn extend_trajectory_rows(
    rows: &mut Vec<TrajectoryRow>,
    all_entries: &[TrajectoryEntry],
    filtered_indices: &[usize],
    start: usize,
    mode: TrajectoryMode,
) {
    let previous = start
        .checked_sub(1)
        .and_then(|index| filtered_indices.get(index))
        .map(|&index| &all_entries[index]);
    let mut previous_turn = previous.and_then(|entry| entry.turn);
    let mut previous_request = previous.and_then(|entry| entry.request);
    let mut request_input_seen = previous_request.is_some();
    rows.reserve(filtered_indices.len().saturating_sub(start));
    for &all_index in &filtered_indices[start..] {
        let entry = &all_entries[all_index];
        if mode == TrajectoryMode::Requests && entry.request != previous_request {
            if let Some(request) = entry.request {
                rows.push(TrajectoryRow::RequestHeader(request));
                request_input_seen = false;
            }
            previous_request = entry.request;
        }
        if mode == TrajectoryMode::Requests && entry.request.is_some() && !request_input_seen {
            if entry.category != "Input" {
                rows.push(TrajectoryRow::Setup);
            }
            request_input_seen = true;
        }
        if mode != TrajectoryMode::Requests && entry.turn != previous_turn {
            if let Some(turn) = entry.turn {
                rows.push(TrajectoryRow::TurnHeader(turn));
            }
            previous_turn = entry.turn;
        }
        rows.push(TrajectoryRow::Entry(all_index));
    }
}

#[derive(Default)]
pub struct TrajectorySummary {
    pub overview_positions: [HashSet<usize>; 3],
    pub overview_prefix: [Vec<u32>; 3],
    pub tool_count: usize,
    pub total_duration_ms: u64,
    pub anomaly_count: usize,
    pub max_turn: u32,
}

pub fn summarize_trajectory(entries: &[TrajectoryEntry]) -> TrajectorySummary {
    let mut summary = TrajectorySummary::default();
    extend_trajectory_summary(&mut summary, entries);
    summary
}

pub fn extend_trajectory_summary(summary: &mut TrajectorySummary, entries: &[TrajectoryEntry]) {
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

pub struct TrajectoryRenderCache {
    pub key: TrajectoryCacheKey,
    pub all_entries: Vec<TrajectoryEntry>,
    pub categories: Arc<Vec<String>>,
    pub lanes: Arc<Vec<String>>,
    pub lane_latest: Arc<BTreeMap<String, String>>,
    pub filtered_indices: Vec<usize>,
    pub previews: Vec<SharedString>,
    pub rows: Vec<TrajectoryRow>,
    pub summary: TrajectorySummary,
}
