use crate::harness::{JsonlStore, Record, SessionStore, ToolExecutionOutcome, ToolExecutionPhase};
use crate::local_tool_router::{needle_engine, needle_model_path, render_needle_candidate};
use crate::types::{AgentMessage, AgentToolDefinition};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct NeedleEvalConfig {
    pub sessions_dir: PathBuf,
    pub tools_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedleEvalDecision {
    Pass,
    Fail,
    Inconclusive,
}

impl NeedleEvalDecision {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::Inconclusive => 2,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NeedleEvalSkipped {
    pub malformed: usize,
    pub text_only: usize,
    pub continuation_only: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub declined: usize,
    pub obsolete_tool: usize,
    pub over_five_label: usize,
}

impl NeedleEvalSkipped {
    pub fn total(&self) -> usize {
        self.malformed
            + self.text_only
            + self.continuation_only
            + self.failed
            + self.cancelled
            + self.declined
            + self.obsolete_tool
            + self.over_five_label
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedleEvalReport {
    pub decision: NeedleEvalDecision,
    pub eligible: usize,
    pub skipped: NeedleEvalSkipped,
    pub top_one_passes: usize,
    pub top_three_passes: usize,
    pub top_five_passes: usize,
    pub p50_latency_us: Option<u64>,
    pub p95_latency_us: Option<u64>,
    pub misses_by_tool: BTreeMap<String, usize>,
    pub model_sha256: String,
    pub catalogue_sha256: String,
}

impl fmt::Display for NeedleEvalReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "decision: {}",
            match self.decision {
                NeedleEvalDecision::Pass => "pass",
                NeedleEvalDecision::Fail => "fail",
                NeedleEvalDecision::Inconclusive => "inconclusive",
            }
        )?;
        writeln!(f, "eligible_turns: {}", self.eligible)?;
        writeln!(f, "skipped_turns: {}", self.skipped.total())?;
        writeln!(f, "skipped_malformed: {}", self.skipped.malformed)?;
        writeln!(f, "skipped_text_only: {}", self.skipped.text_only)?;
        writeln!(
            f,
            "skipped_continuation_only: {}",
            self.skipped.continuation_only
        )?;
        writeln!(f, "skipped_failed: {}", self.skipped.failed)?;
        writeln!(f, "skipped_cancelled: {}", self.skipped.cancelled)?;
        writeln!(f, "skipped_declined: {}", self.skipped.declined)?;
        writeln!(f, "skipped_obsolete_tool: {}", self.skipped.obsolete_tool)?;
        writeln!(
            f,
            "skipped_over_five_label: {}",
            self.skipped.over_five_label
        )?;
        writeln!(
            f,
            "top_one: {} ({})",
            self.top_one_passes,
            percent(self.top_one_passes, self.eligible)
        )?;
        writeln!(
            f,
            "top_three: {} ({})",
            self.top_three_passes,
            percent(self.top_three_passes, self.eligible)
        )?;
        writeln!(
            f,
            "top_five: {} ({})",
            self.top_five_passes,
            percent(self.top_five_passes, self.eligible)
        )?;
        writeln!(
            f,
            "p50_latency_us: {}",
            self.p50_latency_us
                .map_or_else(|| "n/a".into(), |value| value.to_string())
        )?;
        writeln!(
            f,
            "p95_latency_us: {}",
            self.p95_latency_us
                .map_or_else(|| "n/a".into(), |value| value.to_string())
        )?;
        for (tool, misses) in &self.misses_by_tool {
            writeln!(f, "miss_{}: {}", tool, misses)?;
        }
        writeln!(f, "model_sha256: {}", self.model_sha256)?;
        write!(f, "catalogue_sha256: {}", self.catalogue_sha256)
    }
}

struct EvalExample {
    prompt: String,
    expected: BTreeSet<String>,
}

struct ExtractedExamples {
    examples: Vec<EvalExample>,
    skipped: NeedleEvalSkipped,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SkipReason {
    TextOnly,
    ContinuationOnly,
    Failed,
    Cancelled,
    Declined,
    ObsoleteTool,
    OverFiveLabel,
}

impl NeedleEvalSkipped {
    fn add(&mut self, reason: SkipReason) {
        match reason {
            SkipReason::TextOnly => self.text_only += 1,
            SkipReason::ContinuationOnly => self.continuation_only += 1,
            SkipReason::Failed => self.failed += 1,
            SkipReason::Cancelled => self.cancelled += 1,
            SkipReason::Declined => self.declined += 1,
            SkipReason::ObsoleteTool => self.obsolete_tool += 1,
            SkipReason::OverFiveLabel => self.over_five_label += 1,
        }
    }
}

pub fn run_needle_history_eval(config: &NeedleEvalConfig) -> Result<NeedleEvalReport, String> {
    let definitions = read_catalogue(&config.tools_path)?;
    let catalogue_bytes = serde_json::to_vec(&definitions)
        .map_err(|_| "Tool catalogue could not be serialized.".to_string())?;
    let catalogue_sha256 = sha256(&catalogue_bytes);
    let model_path = needle_model_path();
    let model_sha256 =
        sha256(&std::fs::read(&model_path).map_err(|_| "Needle model is unreadable.".to_string())?);
    let engine = needle_engine()?;
    let catalogue_names = definitions.iter().map(|tool| tool.name.clone()).collect();
    let mut extracted = ExtractedExamples {
        examples: Vec::new(),
        skipped: NeedleEvalSkipped::default(),
    };

    for path in session_paths(&config.sessions_dir)? {
        match JsonlStore::open_read_only(path) {
            Ok(store) => {
                let current = extract_store_examples(&store, &catalogue_names);
                extracted.examples.extend(current.examples);
                add_skipped(&mut extracted.skipped, &current.skipped);
            }
            Err(_) => extracted.skipped.malformed += 1,
        }
    }

    let rendered = definitions
        .iter()
        .map(render_needle_candidate)
        .collect::<Vec<_>>();
    let descriptions = rendered.iter().map(String::as_str).collect::<Vec<_>>();
    let mut top_one_passes = 0;
    let mut top_three_passes = 0;
    let mut top_five_passes = 0;
    let mut latencies = Vec::with_capacity(extracted.examples.len());
    let mut misses_by_tool = BTreeMap::new();
    for example in &extracted.examples {
        let started = Instant::now();
        let ranked = engine.retrieve_tools(&example.prompt, &descriptions, 5);
        latencies.push(started.elapsed().as_micros().try_into().unwrap_or(u64::MAX));
        let names = ranked_names(&ranked, &definitions);
        top_one_passes += usize::from(turn_passes(&example.expected, &names[..names.len().min(1)]));
        top_three_passes +=
            usize::from(turn_passes(&example.expected, &names[..names.len().min(3)]));
        if turn_passes(&example.expected, &names) {
            top_five_passes += 1;
        } else {
            for name in &example.expected {
                if !names.iter().any(|candidate| candidate == name) {
                    *misses_by_tool.entry(name.clone()).or_default() += 1;
                }
            }
        }
    }
    latencies.sort_unstable();
    Ok(NeedleEvalReport {
        decision: decision(extracted.examples.len(), top_five_passes),
        eligible: extracted.examples.len(),
        skipped: extracted.skipped,
        top_one_passes,
        top_three_passes,
        top_five_passes,
        p50_latency_us: nearest_rank_percentile(&latencies, 50),
        p95_latency_us: nearest_rank_percentile(&latencies, 95),
        misses_by_tool,
        model_sha256,
        catalogue_sha256,
    })
}

fn read_catalogue(path: &Path) -> Result<Vec<AgentToolDefinition>, String> {
    let bytes = std::fs::read(path).map_err(|_| "Tool catalogue is unreadable.".to_string())?;
    let schemas = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| "Tool catalogue is invalid.".to_string())?;
    let schemas = schemas
        .as_array()
        .ok_or_else(|| "Tool catalogue must be an array.".to_string())?;
    let mut names = HashSet::new();
    let definitions = schemas
        .iter()
        .map(AgentToolDefinition::from_provider_schema)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Tool catalogue contains an invalid tool.".to_string())?;
    if definitions.is_empty() {
        return Err("Tool catalogue is empty.".into());
    }
    if definitions
        .iter()
        .any(|tool| !names.insert(tool.name.clone()))
    {
        return Err("Tool catalogue contains duplicate names.".into());
    }
    Ok(definitions)
}

fn session_paths(sessions_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(sessions_dir)
        .map_err(|_| "Sessions directory is unreadable.".to_string())?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| "Sessions directory is unreadable.".to_string())?;
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|_| "Sessions directory is unreadable.".to_string())?
            .is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".harness.jsonl"))
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn extract_store_examples<S: SessionStore>(
    store: &S,
    catalogue: &HashSet<String>,
) -> ExtractedExamples {
    let mut extracted = ExtractedExamples {
        examples: Vec::new(),
        skipped: NeedleEvalSkipped::default(),
    };
    for lane in store.lanes() {
        let entries = store.transcript(&lane).entries;
        for (start, entry) in entries.iter().enumerate() {
            let prompt = match &entry.message {
                AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                    content
                }
                _ => continue,
            };
            let end = entries[start + 1..]
                .iter()
                .position(|entry| entry.message.is_user())
                .map_or(entries.len(), |offset| start + 1 + offset);
            let turn = &entries[start..end];
            let Some((assistant_index, assistant)) = turn
                .iter()
                .enumerate()
                .find(|(_, entry)| matches!(entry.message, AgentMessage::Assistant { .. }))
            else {
                extracted.skipped.add(SkipReason::TextOnly);
                continue;
            };
            let calls = match &assistant.message {
                AgentMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } if !calls.is_empty() => calls,
                _ => {
                    let continuation = turn[assistant_index + 1..].iter().any(|entry| matches!(&entry.message, AgentMessage::Assistant { tool_calls: Some(calls), .. } if !calls.is_empty()));
                    extracted.skipped.add(if continuation {
                        SkipReason::ContinuationOnly
                    } else {
                        SkipReason::TextOnly
                    });
                    continue;
                }
            };
            let outcome_end = turn[assistant_index + 1..]
                .iter()
                .position(|entry| matches!(entry.message, AgentMessage::Assistant { .. }))
                .map_or(turn.len(), |offset| assistant_index + 1 + offset);
            let outcomes = bounded_outcomes(
                store.records(),
                &lane,
                assistant.seq,
                turn.get(outcome_end)
                    .or_else(|| entries.get(end))
                    .map_or(u64::MAX, |entry| entry.seq),
            );
            let mut expected = BTreeSet::new();
            let mut observed = Vec::new();
            for call in calls {
                let outcome = outcomes
                    .get(&(call.id.clone(), call.function.name.clone()))
                    .cloned()
                    .or_else(|| {
                        legacy_outcome(
                            &turn[assistant_index + 1..outcome_end],
                            &call.id,
                            &call.function.name,
                        )
                    });
                if let Some(outcome) = outcome {
                    if outcome == ToolExecutionOutcome::Succeeded {
                        expected.insert(call.function.name.clone());
                    }
                    observed.push(outcome);
                }
            }
            if expected.is_empty() {
                extracted.skipped.add(skip_for_outcomes(&observed));
            } else if expected.iter().any(|name| !catalogue.contains(name)) {
                extracted.skipped.add(SkipReason::ObsoleteTool);
            } else if expected.len() > 5 {
                extracted.skipped.add(SkipReason::OverFiveLabel);
            } else {
                extracted.examples.push(EvalExample {
                    prompt: prompt.clone(),
                    expected,
                });
            }
        }
    }
    extracted
}

fn bounded_outcomes(
    records: &[Record],
    lane: &str,
    after_seq: u64,
    before_seq: u64,
) -> BTreeMap<(String, String), ToolExecutionOutcome> {
    let mut outcomes = BTreeMap::new();
    for record in records {
        if let Record::ToolExecutionObserved {
            seq,
            lane: record_lane,
            tool_call_id,
            tool_name,
            phase: ToolExecutionPhase::Finished,
            outcome: Some(outcome),
            ..
        } = record
        {
            if record_lane == lane && *seq > after_seq && *seq < before_seq {
                outcomes
                    .entry((tool_call_id.as_str().into(), tool_name.as_str().into()))
                    .or_insert_with(|| outcome.clone());
            }
        }
    }
    outcomes
}

fn legacy_outcome(
    entries: &[crate::harness::Entry],
    call_id: &str,
    tool_name: &str,
) -> Option<ToolExecutionOutcome> {
    entries.iter().find_map(|entry| match &entry.message {
        AgentMessage::Tool {
            tool_call_id,
            name,
            is_error,
            ..
        } if tool_call_id == call_id && name == tool_name => Some(if *is_error {
            ToolExecutionOutcome::Failed
        } else {
            ToolExecutionOutcome::Succeeded
        }),
        _ => None,
    })
}

fn skip_for_outcomes(outcomes: &[ToolExecutionOutcome]) -> SkipReason {
    if outcomes
        .iter()
        .any(|outcome| *outcome == ToolExecutionOutcome::Failed)
    {
        SkipReason::Failed
    } else if outcomes
        .iter()
        .any(|outcome| *outcome == ToolExecutionOutcome::Cancelled)
    {
        SkipReason::Cancelled
    } else if outcomes
        .iter()
        .any(|outcome| *outcome == ToolExecutionOutcome::Declined)
    {
        SkipReason::Declined
    } else {
        SkipReason::Failed
    }
}

fn ranked_names(ranked: &[(usize, f32)], definitions: &[AgentToolDefinition]) -> Vec<String> {
    let mut seen = HashSet::new();
    ranked
        .iter()
        .filter_map(|(index, _)| definitions.get(*index).filter(|_| seen.insert(*index)))
        .map(|tool| tool.name.clone())
        .take(5)
        .collect()
}

fn turn_passes(expected: &BTreeSet<String>, ranked: &[String]) -> bool {
    expected
        .iter()
        .all(|tool| ranked.iter().any(|candidate| candidate == tool))
}

fn decision(eligible: usize, top_five_passes: usize) -> NeedleEvalDecision {
    if eligible < 200 {
        NeedleEvalDecision::Inconclusive
    } else if top_five_passes.saturating_mul(100) >= eligible.saturating_mul(99) {
        NeedleEvalDecision::Pass
    } else {
        NeedleEvalDecision::Fail
    }
}

fn nearest_rank_percentile(sorted: &[u64], percentile: u64) -> Option<u64> {
    let rank = sorted
        .len()
        .saturating_mul(percentile as usize)
        .div_ceil(100)
        .max(1);
    sorted.get(rank - 1).copied()
}

fn add_skipped(total: &mut NeedleEvalSkipped, current: &NeedleEvalSkipped) {
    total.malformed += current.malformed;
    total.text_only += current.text_only;
    total.continuation_only += current.continuation_only;
    total.failed += current.failed;
    total.cancelled += current.cancelled;
    total.declined += current.declined;
    total.obsolete_tool += current.obsolete_tool;
    total.over_five_label += current.over_five_label;
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn percent(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "n/a".into()
    } else {
        format!("{:.2}%", numerator as f64 * 100.0 / denominator as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{
        MemoryStore, Record, SessionStore, ToolExecutionOutcome, ToolExecutionPhase, TraceString,
    };
    use crate::types::AgentMessage;
    use std::collections::{BTreeSet, HashSet};
    use threadlane_protocol::{RuntimeToolCall, RuntimeToolCallFunction};

    fn assistant_call(id: &str, name: &str) -> AgentMessage {
        AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![RuntimeToolCall {
                id: id.into(),
                r#type: "function".into(),
                function: RuntimeToolCallFunction {
                    name: name.into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }]),
            stop_reason: None,
            deferred_handle: None,
        }
    }

    fn successful_tool(id: &str, name: &str) -> AgentMessage {
        AgentMessage::Tool {
            tool_call_id: id.into(),
            name: name.into(),
            content: "result".into(),
            is_error: false,
            terminate: false,
        }
    }

    #[test]
    fn extracts_only_successful_first_assistant_tools() {
        let mut store = MemoryStore::new("session");
        let user = store.append_message(
            None,
            AgentMessage::User {
                content: "find rust files".into(),
            },
        );
        let assistant = store.append_message(Some(user), assistant_call("call-1", "search_files"));
        store.append_message(Some(assistant), successful_tool("call-1", "search_files"));

        let catalog = HashSet::from(["search_files".to_string()]);
        let extracted = extract_store_examples(&store, &catalog);
        assert_eq!(extracted.examples.len(), 1);
        assert_eq!(
            extracted.examples[0].expected,
            BTreeSet::from(["search_files".into()])
        );
    }

    #[test]
    fn skips_failed_continuation_obsolete_and_over_five_turns() {
        let catalog = HashSet::from([
            "search".to_string(),
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
            "five".to_string(),
            "six".to_string(),
        ]);
        let mut store = MemoryStore::new("session");

        let failed_user = store.append_message(
            None,
            AgentMessage::User {
                content: "failed".into(),
            },
        );
        let failed_assistant =
            store.append_message(Some(failed_user), assistant_call("failed", "search"));
        store.append_message(
            Some(failed_assistant),
            AgentMessage::Tool {
                tool_call_id: "failed".into(),
                name: "search".into(),
                content: "error".into(),
                is_error: true,
                terminate: false,
            },
        );

        let continuation_user = store.append_message(
            None,
            AgentMessage::User {
                content: "continued".into(),
            },
        );
        let continuation_assistant = store.append_message(
            Some(continuation_user),
            AgentMessage::Assistant {
                content: Some("text".into()),
                tool_calls: Some(Vec::new()),
                stop_reason: None,
                deferred_handle: None,
            },
        );
        let continuation_call = store.append_message(
            Some(continuation_assistant),
            assistant_call("later", "search"),
        );
        store.append_message(Some(continuation_call), successful_tool("later", "search"));

        let obsolete_user = store.append_message(
            None,
            AgentMessage::User {
                content: "obsolete".into(),
            },
        );
        let obsolete_assistant =
            store.append_message(Some(obsolete_user), assistant_call("obsolete", "gone"));
        store.append_message(
            Some(obsolete_assistant),
            successful_tool("obsolete", "gone"),
        );

        let many_user = store.append_message(
            None,
            AgentMessage::User {
                content: "many".into(),
            },
        );
        let many_assistant = store.append_message(
            Some(many_user),
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(
                    ["one", "two", "three", "four", "five", "six"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, name)| RuntimeToolCall {
                            id: format!("many-{index}"),
                            r#type: "function".into(),
                            function: RuntimeToolCallFunction {
                                name: name.into(),
                                arguments: "{}".into(),
                            },
                            thought_signature: None,
                        })
                        .collect(),
                ),
                stop_reason: None,
                deferred_handle: None,
            },
        );
        for (index, name) in ["one", "two", "three", "four", "five", "six"]
            .into_iter()
            .enumerate()
        {
            store.append_message(
                Some(many_assistant.clone()),
                successful_tool(&format!("many-{index}"), name),
            );
        }

        let extracted = extract_store_examples(&store, &catalog);
        assert!(extracted.examples.is_empty());
        assert_eq!(extracted.skipped.failed, 1);
        assert_eq!(extracted.skipped.continuation_only, 1);
        assert_eq!(extracted.skipped.obsolete_tool, 1);
        assert_eq!(extracted.skipped.over_five_label, 1);
    }

    #[test]
    fn deduplicates_repeated_tool_results_for_one_prompt() {
        let mut store = MemoryStore::new("session");
        let user = store.append_message(
            None,
            AgentMessage::User {
                content: "find".into(),
            },
        );
        let assistant = store.append_message(Some(user), assistant_call("call", "search"));
        store.append_message(Some(assistant.clone()), successful_tool("call", "search"));
        store.append_message(Some(assistant), successful_tool("call", "search"));

        let extracted = extract_store_examples(&store, &HashSet::from(["search".to_string()]));
        assert_eq!(
            extracted.examples[0].expected,
            BTreeSet::from(["search".into()])
        );
    }

    #[test]
    fn bounds_reused_call_ids_and_prefers_durable_outcomes() {
        let mut store = MemoryStore::new("session");
        let first_user = store.append_message(
            None,
            AgentMessage::User {
                content: "first".into(),
            },
        );
        let first_assistant =
            store.append_message(Some(first_user), assistant_call("shared", "search"));
        store.append_message(Some(first_assistant), successful_tool("shared", "search"));
        store.append_record_unchecked(Record::ToolExecutionObserved {
            id: "observed".into(),
            seq: store.next_sequence(),
            lane: "main".into(),
            timestamp: 1,
            run_id: "run".into(),
            attempt: Some(1),
            tool_call_id: TraceString::new("shared").unwrap(),
            tool_name: TraceString::new("search").unwrap(),
            executor_kind: TraceString::new("test").unwrap(),
            phase: ToolExecutionPhase::Finished,
            started_at_ms: None,
            duration_ms: None,
            outcome: Some(ToolExecutionOutcome::Failed),
            exit_code: Some(1),
            cancelled: false,
            is_error: Some(true),
            terminate: Some(false),
            output_sha256: None,
            output_bytes: None,
        });
        let second_user = store.append_message(
            None,
            AgentMessage::User {
                content: "second".into(),
            },
        );
        let second_assistant =
            store.append_message(Some(second_user), assistant_call("shared", "search"));
        store.append_message(Some(second_assistant), successful_tool("shared", "search"));

        let extracted = extract_store_examples(&store, &HashSet::from(["search".to_string()]));
        assert_eq!(extracted.examples.len(), 1);
        assert_eq!(
            extracted.examples[0].expected,
            BTreeSet::from(["search".into()])
        );
        assert_eq!(extracted.skipped.failed, 1);
    }

    #[test]
    fn later_same_turn_call_id_cannot_overwrite_first_assistant_outcome() {
        let mut store = MemoryStore::new("session");
        let user = store.append_message(None, AgentMessage::user("find", Vec::new()));
        let first_assistant = store.append_message(Some(user), assistant_call("shared", "search"));
        store.append_record_unchecked(Record::ToolExecutionObserved {
            id: "first-observed".into(),
            seq: store.next_sequence(),
            lane: "main".into(),
            timestamp: 1,
            run_id: "run".into(),
            attempt: Some(1),
            tool_call_id: TraceString::new("shared").unwrap(),
            tool_name: TraceString::new("search").unwrap(),
            executor_kind: TraceString::new("test").unwrap(),
            phase: ToolExecutionPhase::Finished,
            started_at_ms: None,
            duration_ms: None,
            outcome: Some(ToolExecutionOutcome::Succeeded),
            exit_code: Some(0),
            cancelled: false,
            is_error: Some(false),
            terminate: Some(false),
            output_sha256: None,
            output_bytes: None,
        });
        let first_result =
            store.append_message(Some(first_assistant), successful_tool("shared", "search"));
        store.append_message(Some(first_result), assistant_call("shared", "write_file"));
        store.append_record_unchecked(Record::ToolExecutionObserved {
            id: "later-observed".into(),
            seq: store.next_sequence(),
            lane: "main".into(),
            timestamp: 2,
            run_id: "run".into(),
            attempt: Some(2),
            tool_call_id: TraceString::new("shared").unwrap(),
            tool_name: TraceString::new("write_file").unwrap(),
            executor_kind: TraceString::new("test").unwrap(),
            phase: ToolExecutionPhase::Finished,
            started_at_ms: None,
            duration_ms: None,
            outcome: Some(ToolExecutionOutcome::Failed),
            exit_code: Some(1),
            cancelled: false,
            is_error: Some(true),
            terminate: Some(false),
            output_sha256: None,
            output_bytes: None,
        });

        let extracted = extract_store_examples(&store, &HashSet::from(["search".to_string()]));
        assert_eq!(extracted.examples.len(), 1);
        assert_eq!(
            extracted.examples[0].expected,
            BTreeSet::from(["search".into()])
        );
    }

    #[test]
    fn legacy_outcome_does_not_cross_the_next_assistant() {
        let mut store = MemoryStore::new("session");
        let user = store.append_message(None, AgentMessage::user("find", Vec::new()));
        let first_assistant = store.append_message(Some(user), assistant_call("shared", "search"));
        let continuation =
            store.append_message(Some(first_assistant), assistant_call("shared", "search"));
        store.append_message(Some(continuation), successful_tool("shared", "search"));

        let extracted = extract_store_examples(&store, &HashSet::from(["search".to_string()]));
        assert!(extracted.examples.is_empty());
        assert_eq!(extracted.skipped.failed, 1);
    }

    #[test]
    fn strict_recall_requires_every_expected_tool() {
        let expected = BTreeSet::from(["read_file".into(), "search".into()]);
        assert!(!turn_passes(&expected, &["read_file".into()]));
        assert!(turn_passes(
            &expected,
            &["read_file".into(), "search".into()]
        ));
    }

    #[test]
    fn decision_requires_two_hundred_examples_and_ninety_nine_percent() {
        assert_eq!(decision(199, 199), NeedleEvalDecision::Inconclusive);
        assert_eq!(decision(200, 198), NeedleEvalDecision::Pass);
        assert_eq!(decision(200, 197), NeedleEvalDecision::Fail);
    }

    #[test]
    fn nearest_rank_percentiles_are_deterministic() {
        assert_eq!(nearest_rank_percentile(&[1, 2, 3, 4, 100], 50), Some(3));
        assert_eq!(nearest_rank_percentile(&[1, 2, 3, 4, 100], 95), Some(100));
    }
}
