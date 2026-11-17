//! Read-only Normal/Fusion comparison data from existing durable journals.
//! Run: cargo run -p threadlane-coding-agent --example fusion_metrics -- session.jsonl ...
//! Completion is not acceptance. Dollar cost and review quality require external
//! pricing and evaluation; null means unknown, never zero. Provider duration is
//! summed request time, not wall time (parallel requests can overlap).
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use threadlane_protocol::TokenUsage;
use threadlane_runtime::harness::{JsonlStore, OperationOutcome, Record};

#[derive(Default, Serialize)]
struct RunMetrics {
    models: BTreeSet<String>,
    requests_started: u64,
    requests_finished: u64,
    requests_with_usage: u64,
    requests_with_duration: u64,
    reported_usage: TokenUsage,
    provider_duration_ms: u64,
    started_at_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    outcome: Option<OperationOutcome>,
    fusion_events: BTreeMap<String, u64>,
}

fn summarize(records: &[Record]) -> BTreeMap<(String, String), RunMetrics> {
    let mut runs = BTreeMap::<(String, String), RunMetrics>::new();
    for record in records {
        match record {
            Record::OperationStarted {
                lane,
                id,
                wall_time_ms,
                ..
            } => {
                runs.entry((lane.clone(), id.clone()))
                    .or_default()
                    .started_at_ms = *wall_time_ms;
            }
            Record::ProviderRequestStarted {
                lane,
                run_id,
                model,
                ..
            } => {
                let run = runs.entry((lane.clone(), run_id.clone())).or_default();
                run.requests_started += 1;
                run.models.insert(model.as_str().to_owned());
            }
            Record::ProviderRequestFinished {
                lane,
                run_id,
                usage,
                duration_ms,
                ..
            } => {
                let run = runs.entry((lane.clone(), run_id.clone())).or_default();
                run.requests_finished += 1;
                if let Some(usage) = usage {
                    run.requests_with_usage += 1;
                    run.reported_usage.accumulate(usage);
                }
                if let Some(duration) = duration_ms {
                    run.requests_with_duration += 1;
                    run.provider_duration_ms = run.provider_duration_ms.saturating_add(*duration);
                }
            }
            Record::OperationFinished {
                lane,
                run_id,
                outcome,
                wall_time_ms,
                ..
            } => {
                let run = runs.entry((lane.clone(), run_id.clone())).or_default();
                run.outcome = Some(outcome.clone());
                run.finished_at_ms = *wall_time_ms;
            }
            Record::FactSet {
                lane,
                run_id: Some(run_id),
                key,
                value,
                ..
            } if key.starts_with("fusion_audit:") => {
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(value) {
                    if let Some(kind) = event.get("kind").and_then(|kind| kind.as_str()) {
                        let run = runs.entry((lane.clone(), run_id.clone())).or_default();
                        *run.fusion_events.entry(kind.to_owned()).or_default() += 1;
                    }
                }
            }
            _ => {}
        }
    }
    runs
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.is_empty() {
        return Err("usage: fusion_metrics session.jsonl [session.jsonl ...]".into());
    }
    for path in paths {
        let store = JsonlStore::open_read_only(&path)?;
        let runs: Vec<_> = summarize(store.records())
            .into_iter()
            .map(|((lane, run_id), metrics)| {
                serde_json::json!({
                    "lane": lane,
                    "run_id": run_id,
                    "metrics": metrics,
                    "estimated_cost_usd": null,
                    "accepted": null,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "session": path,
                "runs": runs,
            }))?
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_runtime::harness::ProviderOutcome;

    #[test]
    fn counts_failed_and_missing_usage_without_double_counting_run_totals() {
        let finish = |id: &str, lane: &str, usage, outcome| Record::ProviderRequestFinished {
            id: id.into(),
            seq: 1,
            lane: lane.into(),
            timestamp: 0,
            run_id: "run".into(),
            attempt: 1,
            request_id: None,
            outcome,
            error: None,
            duration_ms: Some(12),
            usage,
        };
        let records = vec![
            Record::Usage {
                id: "usage".into(),
                seq: 0,
                lane: "child".into(),
                timestamp: 0,
                run_id: Some("run".into()),
                cause: Default::default(),
                entry_id: None,
                tool_call_id: None,
                attempt: None,
                usage: TokenUsage {
                    total_tokens: 3,
                    ..Default::default()
                },
            },
            finish(
                "1",
                "main",
                Some(TokenUsage {
                    input_tokens: 10,
                    cache_read_tokens: 4,
                    total_tokens: 10,
                    ..Default::default()
                }),
                ProviderOutcome::Completed,
            ),
            finish(
                "2",
                "child",
                Some(TokenUsage {
                    output_tokens: 3,
                    total_tokens: 3,
                    ..Default::default()
                }),
                ProviderOutcome::Failed,
            ),
            finish("3", "child", None, ProviderOutcome::Failed),
        ];
        let runs = summarize(&records);
        let main = &runs[&("main".into(), "run".into())];
        assert_eq!(main.reported_usage.cache_read_tokens, 4);
        let child = &runs[&("child".into(), "run".into())];
        assert_eq!(child.requests_finished, 2);
        assert_eq!(child.requests_with_usage, 1);
        assert_eq!(child.reported_usage.total_tokens, 3);
        assert_eq!(child.provider_duration_ms, 24);
        assert!(child.outcome.is_none());
    }
}
