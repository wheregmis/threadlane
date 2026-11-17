//! Read-only token accounting over existing durable request traces.

use super::{
    ContextItemSource, ContextItemStatus, ContextSnapshotLoadOutcome, OperationIntent,
    OperationOutcome, ProviderOutcome, Record, SessionStore, UsageCause,
};
use std::collections::{HashMap, HashSet};

pub use threadlane_protocol::efficiency::TokenEfficiencyReport;

/// Does not read source bodies or change the journal. Legacy Usage records
/// are used only for runs without per-request usage, avoiding double counting.
pub fn project_token_efficiency<S: SessionStore>(store: &S) -> TokenEfficiencyReport {
    let mut report = TokenEfficiencyReport::default();
    let mut traced_runs = HashSet::new();
    let mut manifests = HashMap::new();
    let mut finished = HashSet::new();
    let mut seen_content = HashSet::new();
    let mut seen_snapshots = HashSet::new();
    let foreground_runs: HashSet<_> = store
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::OperationStarted {
                id,
                lane,
                intent: OperationIntent::Run,
                ..
            } if lane == "main" => Some(id.as_str()),
            _ => None,
        })
        .collect();
    for record in store.records() {
        let lane = report.lanes.entry(record.lane().to_owned()).or_default();
        match record {
            Record::ProviderRequestStarted { .. } => lane.provider_requests += 1,
            Record::ProviderRequestFinished {
                lane: lane_id,
                run_id,
                attempt,
                outcome,
                usage,
                ..
            } if finished.insert((lane_id, run_id, attempt)) => {
                if !matches!(outcome, ProviderOutcome::Completed) {
                    lane.failed_provider_requests += 1;
                }
                if let Some(usage) = usage {
                    traced_runs.insert((lane_id, run_id));
                    lane.requests_with_usage += 1;
                    lane.usage.add(usage);
                    report.usage.add(usage);
                }
            }
            Record::ContextManifestCaptured {
                lane: lane_id,
                run_id,
                attempt,
                total_estimated_tokens,
                items,
                ..
            } => {
                manifests.insert((lane_id, run_id, attempt), *total_estimated_tokens);
                let mut current_content = HashSet::new();
                for item in items
                    .iter()
                    .filter(|item| item.status != ContextItemStatus::Omitted)
                {
                    if item.status == ContextItemStatus::Truncated {
                        lane.reduced_context_items += 1;
                    }
                    let source = match item.source {
                        ContextItemSource::SystemPrompt => "system_prompt",
                        ContextItemSource::ToolSchema => "tool_schema",
                        ContextItemSource::ToolResult => "tool_result",
                        _ => "messages_and_context",
                    };
                    let key = (lane_id, source, item.digest_sha256.as_str());
                    let contribution = lane.context.entry(source.into()).or_default();
                    contribution.estimated_tokens += u64::from(item.token_estimate);
                    if seen_content.contains(&key) {
                        contribution.repeated_estimated_tokens += u64::from(item.token_estimate);
                    }
                    current_content.insert(key);
                }
                seen_content.extend(current_content);
            }
            Record::ContextSnapshotIndexed { snapshot, .. } => {
                if !seen_snapshots.insert((
                    &snapshot.source_lane,
                    &snapshot.path,
                    snapshot.start_line,
                    snapshot.end_line,
                    snapshot.file_sha256.as_str(),
                )) {
                    report.repeated_snapshot_reads += 1;
                }
            }
            Record::ContextSnapshotLoaded { outcome, .. } => {
                report.snapshot_loads += 1;
                if *outcome == ContextSnapshotLoadOutcome::Stale {
                    report.stale_snapshot_loads += 1;
                }
            }
            Record::ContextCompacted { .. } => report.compactions += 1,
            Record::OperationFinished {
                lane,
                run_id,
                outcome: OperationOutcome::Completed,
                ..
            } if lane == "main" && foreground_runs.contains(run_id.as_str()) => {
                report.completed_foreground_runs += 1;
            }
            _ => {}
        }
    }
    // Join by durable lane/run/attempt, regardless of record ordering.
    let mut calibrated = HashSet::new();
    for record in store.records() {
        match record {
            Record::ProviderRequestFinished {
                lane,
                run_id,
                attempt,
                usage: Some(usage),
                ..
            } if calibrated.insert((lane, run_id, attempt)) => {
                if let Some(Some(estimate)) = manifests.get(&(lane, run_id, attempt)) {
                    report.calibrated_requests += 1;
                    report.calibrated_estimated_input_tokens += u64::from(*estimate);
                    report.calibrated_actual_input_tokens += u64::from(usage.input_tokens)
                        + u64::from(usage.cache_read_tokens)
                        + u64::from(usage.cache_write_tokens);
                }
            }
            Record::Usage {
                lane,
                run_id,
                cause: UsageCause::Provider | UsageCause::Discarded,
                usage,
                ..
            } if !run_id
                .as_ref()
                .is_some_and(|run| traced_runs.contains(&(lane, run))) =>
            {
                report
                    .lanes
                    .entry(lane.clone())
                    .or_default()
                    .usage
                    .add(usage);
                report.usage.add(usage);
            }
            _ => {}
        }
    }
    if report.completed_foreground_runs > 0 {
        report.session_tokens_per_completed_foreground_run =
            Some(report.usage.processed_tokens() as f64 / report.completed_foreground_runs as f64);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::project_token_efficiency;
    use crate::harness::{MemoryStore, Record, SessionStore};
    use serde_json::json;

    #[test]
    fn report_counts_children_failures_and_legacy_usage_without_double_counting() {
        let mut store = MemoryStore::new("efficiency");
        let mut append = |lane: &str, kind: &str, mut fields: serde_json::Value| {
            let seq = store.next_sequence();
            let fields = fields.as_object_mut().unwrap();
            fields.insert(
                "id".into(),
                json!(if kind == "OperationStarted" {
                    lane.to_owned()
                } else {
                    format!("record-{seq}")
                }),
            );
            fields.insert("seq".into(), json!(seq));
            fields.insert("timestamp".into(), json!(seq));
            fields.insert("lane".into(), json!(lane));
            let record: Record = serde_json::from_value(json!({kind: fields})).unwrap();
            SessionStore::append_record(&mut store, record).unwrap();
        };
        let usage = json!({"input_tokens": 100, "output_tokens": 20,
            "cache_read_tokens": 50, "cache_write_tokens": 5, "total_tokens": 175});
        for lane in ["main", "child"] {
            append(lane, "OperationStarted", json!({"intent": "Run"}));
            for attempt in 1..=2 {
                append(
                    lane,
                    "ProviderRequestStarted",
                    json!({"run_id": lane,
                    "attempt": attempt, "provider": "test", "model": "test"}),
                );
                append(
                    lane,
                    "ContextManifestCaptured",
                    json!({"run_id": lane,
                    "attempt": attempt, "request_id": format!("{lane}-{attempt}"),
                    "total_estimated_tokens": 200, "items": [{"position": 0,
                    "source": "ToolResult", "role": "tool", "token_estimate": 100,
                    "status": "Active", "digest_sha256": "a".repeat(64)}]}),
                );
                append(
                    lane,
                    "ProviderRequestFinished",
                    json!({"run_id": lane,
                    "attempt": attempt, "outcome": if attempt == 1 { "Failed" } else { "Completed" },
                    "usage": usage}),
                );
            }
            // A cumulative compatibility ledger must not count these again.
            append(
                lane,
                "Usage",
                json!({"run_id": lane, "cause": "Provider", "usage": usage}),
            );
        }
        append("legacy", "OperationStarted", json!({"intent": "Run"}));
        append(
            "legacy",
            "Usage",
            json!({"cause": "Provider", "usage": usage}),
        );
        let report = project_token_efficiency(&store);
        assert_eq!(report.usage.uncached_input_tokens, 500);
        assert_eq!(report.usage.cache_read_tokens, 250);
        assert_eq!(report.usage.cache_write_tokens, 25);
        assert_eq!(report.usage.output_tokens, 100);
        assert_eq!(report.lanes["child"].failed_provider_requests, 1);
        assert_eq!(
            report.lanes["main"].context["tool_result"].repeated_estimated_tokens,
            100
        );
        assert_eq!(
            report.lanes["child"].context["tool_result"].repeated_estimated_tokens,
            100
        );
        assert_eq!(report.calibrated_requests, 4);
        assert_eq!(report.calibrated_actual_input_tokens, 620);
        assert_eq!(report.calibrated_estimated_input_tokens, 800);
        assert_eq!(report.session_tokens_per_completed_foreground_run, None);
    }
}
