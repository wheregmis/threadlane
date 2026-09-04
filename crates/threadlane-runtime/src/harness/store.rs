use super::types::{Entry, Record, ReduceError, ReducedState};
use crate::types::{AgentMessage, TokenUsage};
use std::collections::BTreeSet;

/// A deterministic, model-facing projection of the canonical event log.
///
/// Operational records remain durable in the same store, but never appear in
/// this projection. Consumers that construct provider requests should use this
/// type rather than walking the session log or a rendered transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelContextProjection {
    lane: String,
    pub leaf_id: Option<String>,
    /// Selected active branch entries, including a compaction checkpoint and
    /// only the tail after that checkpoint.
    pub entries: Vec<Entry>,
    /// The latest durable compaction checkpoint in `entries`, if selected.
    pub checkpoint: Option<CompactionCheckpoint>,
}

/// Metadata derived from a durable compaction summary entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionCheckpoint {
    pub entry_id: String,
    seq: u64,
    compacted_messages: Option<usize>,
    source_leaf_id: Option<String>,
}

impl ModelContextProjection {
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.entries
            .iter()
            .map(|entry| entry.message.clone())
            .collect()
    }
}

/// The ordered, user-facing transcript projection for a lane.
///
/// Unlike [`ModelContextProjection`], this projection intentionally retains
/// every durable entry in chronological sequence order. It is suitable for UI
/// reconciliation and audits, not for provider payloads.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptProjection {
    lane: String,
    pub entries: Vec<Entry>,
}

impl TranscriptProjection {
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.entries
            .iter()
            .map(|entry| entry.message.clone())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdGenerator {
    session_id: String,
}

impl SessionIdGenerator {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }

    pub fn next(&self, kind: &str, used_ids: &[String]) -> String {
        let session = self
            .session_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let kind = kind
            .trim()
            .replace(|character: char| !character.is_ascii_alphanumeric(), "-");
        let base = format!("{session}-{kind}");
        let mut counter = 1u64;
        loop {
            let candidate = format!("{base}-{counter}");
            if !used_ids.iter().any(|used| used == &candidate) {
                return candidate;
            }
            counter = counter.saturating_add(1);
        }
    }
}

/// The smallest persistence contract needed by harness procedures.
///
/// Implementations must commit the append before returning.  A failed append
/// must not expose a new entry or record through the read methods.
pub trait SessionStore {
    fn session_id(&self) -> &str;
    fn reduced_state(&self) -> Option<ReducedState> {
        None
    }
    /// Commits a related group as one durable unit. Stores that do not support
    /// atomic append groups reject the operation rather than exposing a prefix.
    fn append_actions_atomically(
        &mut self,
        _actions: &[super::EffectAction],
    ) -> Result<(), ReduceError> {
        Err(ReduceError::Storage(
            "atomic append groups are not supported by this store".into(),
        ))
    }
    fn next_sequence(&self) -> u64 {
        self.entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.records().iter().map(Record::seq))
            .max()
            .unwrap_or(0)
            + 1
    }
    fn refresh(&mut self) -> Result<(), ReduceError> {
        Ok(())
    }
    fn facts(&self) -> std::collections::BTreeMap<String, String> {
        let mut facts = std::collections::BTreeMap::new();
        for record in self.records() {
            if let Record::FactSet {
                key,
                value,
                run_id: None,
                ..
            } = record
            {
                facts.insert(key.clone(), value.clone());
            }
        }
        facts
    }
    fn entries(&self) -> &[Entry];
    fn records(&self) -> &[Record];
    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries().iter().find(|entry| entry.id == id)
    }
    fn lanes(&self) -> Vec<String> {
        let mut lanes = BTreeSet::from([String::from("main")]);
        lanes.extend(self.entries().iter().map(|entry| entry.lane.clone()));
        lanes.extend(self.records().iter().map(|record| record.lane().to_owned()));
        lanes.into_iter().collect()
    }
    fn preferred_leaf(&self, _lane: &str) -> Option<String> {
        None
    }
    fn name(&self) -> Option<String> {
        self.facts().get("name").cloned()
    }
    fn has_name(&self) -> bool {
        self.name().as_ref().is_some_and(|n| !n.trim().is_empty())
    }
    fn model(&self) -> Option<String> {
        self.facts().get("model").cloned()
    }
    fn parent_session_id(&self) -> Option<String> {
        self.facts().get("parent_session_id").cloned()
    }
    fn title_attempted(&self) -> bool {
        self.facts()
            .get("title_attempted")
            .map_or(false, |v| v == "true")
    }
    fn plan(&self) -> crate::types::SessionPlan {
        if let Some(plan_json) = self.facts().get("session_plan") {
            if let Ok(plan) = serde_json::from_str::<crate::types::SessionPlan>(plan_json) {
                return plan;
            }
        }
        crate::types::SessionPlan::default()
    }
    fn active_branch_messages(&self, lane: &str) -> Vec<AgentMessage>
    where
        Self: Sized,
    {
        self.model_context(lane)
            .map(|ctx| ctx.messages())
            .unwrap_or_default()
    }
    fn get_persisted_messages(&self) -> Vec<AgentMessage> {
        self.entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect()
    }
    fn mark_title_attempted(&mut self) -> Result<bool, ReduceError> {
        if self.title_attempted() {
            return Ok(false);
        }
        self.append_fact("main", "title_attempted", "true", None)?;
        Ok(true)
    }
    fn set_name(&mut self, name: impl Into<String>) -> Result<(), ReduceError> {
        let name = name.into();
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(ReduceError::InvalidRecord(
                "session name cannot be empty".into(),
            ));
        }
        self.append_fact("main", "name", trimmed, None)
    }
    fn set_model(&mut self, model: impl Into<String>) -> Result<(), ReduceError> {
        let model = model.into();
        let trimmed = model.trim();
        if trimmed.is_empty() {
            return Err(ReduceError::InvalidRecord(
                "session model cannot be empty".into(),
            ));
        }
        self.append_fact("main", "model", trimmed, None)
    }
    /// Build the model-facing context projection for one lane's selected branch.
    ///
    /// The append-only log remains the source of truth. This projection selects
    /// only model-visible entries from the active branch; lifecycle, telemetry,
    /// permission, and recovery records stay available for other projections.
    fn model_context(&self, lane: &str) -> Result<ModelContextProjection, ReduceError>
    where
        Self: Sized,
    {
        let state = super::Reducer::reduce(self)?;
        let leaf_id = state
            .lane(lane)
            .and_then(|lane| lane.leaf_id.clone())
            .or_else(|| self.preferred_leaf(lane));
        let raw_entries: Vec<Entry> = self
            .branch(leaf_id.as_deref(), usize::MAX)
            .into_iter()
            .filter(|entry| entry.lane == lane)
            .collect();
        let mut entries: Vec<Entry> = Vec::new();
        for entry in raw_entries {
            match &entry.surface_op {
                super::types::SurfaceOperation::Append => {
                    entries.push(entry);
                }
                super::types::SurfaceOperation::Replace {
                    start_seq, end_seq, ..
                } => {
                    entries.retain(|e| e.seq < *start_seq || e.seq > *end_seq);
                    entries.push(entry);
                }
            }
        }
        let checkpoint = entries.iter().rev().find_map(|entry| match &entry.message {
            AgentMessage::Custom {
                custom_type,
                payload,
            } if custom_type == "compaction_summary" => Some(CompactionCheckpoint {
                entry_id: entry.id.clone(),
                seq: entry.seq,
                compacted_messages: payload
                    .get("compacted_messages")
                    .and_then(|value| value.as_u64())
                    .and_then(|value| usize::try_from(value).ok()),
                source_leaf_id: payload
                    .get("source_leaf_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
            }),
            _ => None,
        });
        Ok(ModelContextProjection {
            lane: lane.to_owned(),
            leaf_id,
            entries,
            checkpoint,
        })
    }

    /// Build the chronological transcript projection for one lane.
    ///
    /// This retains inactive and compacted branch entries so the UI can show a
    /// complete durable history without accidentally using it as model context.
    fn transcript(&self, lane: &str) -> TranscriptProjection {
        let mut entries = self
            .entries()
            .iter()
            .filter(|entry| entry.lane == lane)
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.seq);
        TranscriptProjection {
            lane: lane.to_owned(),
            entries,
        }
    }

    fn branch(&self, leaf_id: Option<&str>, limit: usize) -> Vec<Entry> {
        if limit == 0 {
            return Vec::new();
        }
        let mut branch = Vec::new();
        let mut current = leaf_id
            .and_then(|id| self.entry(id))
            .or_else(|| self.entries().last());
        while let Some(entry) = current {
            branch.push(entry.clone());
            if branch.len() == limit {
                break;
            }
            current = entry.parent_id.as_deref().and_then(|id| self.entry(id));
        }
        branch.reverse();
        branch
    }
    fn lane_log(&self, lane: &str, after_seq: u64, limit: usize) -> Vec<Record> {
        self.records()
            .iter()
            .filter(|record| record.lane() == lane && record.seq() > after_seq)
            .take(limit)
            .cloned()
            .collect()
    }
    fn usage_sum(&self, lane: &str) -> TokenUsage {
        let mut total = TokenUsage::default();
        for record in self.records().iter().filter(|record| record.lane() == lane) {
            if let Record::Usage { usage, .. } = record {
                total.accumulate(usage);
            }
        }
        total
    }
    fn append_fact(
        &mut self,
        lane: &str,
        key: &str,
        value: &str,
        run_id: Option<&str>,
    ) -> Result<(), ReduceError> {
        if lane.trim().is_empty() || key.trim().is_empty() {
            return Err(ReduceError::InvalidRecord(
                "fact lane and key must be non-empty".into(),
            ));
        }
        let seq = self
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.records().iter().map(Record::seq))
            .max()
            .unwrap_or(0)
            + 1;
        self.append_record(Record::FactSet {
            id: format!("fact-{lane}-{key}-{seq}"),
            seq,
            lane: lane.into(),
            timestamp: seq,
            run_id: run_id.map(str::to_owned),
            key: key.into(),
            value: value.into(),
        })
    }
    fn append_entry(&mut self, entry: Entry) -> Result<(), ReduceError>;
    fn append_record(&mut self, record: Record) -> Result<(), ReduceError>;
}

#[cfg(test)]
mod tests {
    use super::{SessionIdGenerator, SessionStore};
    use crate::harness::{MemoryStore, Record, Reducer, UsageCause};
    use crate::types::{AgentMessage, TokenUsage};

    #[test]
    fn reference_queries_are_bounded_and_consistent() {
        let mut store = MemoryStore::new("session-1");
        let first = store.append_message(None, AgentMessage::user("one", vec![]));
        let second = store.append_message(Some(first.clone()), AgentMessage::user("two", vec![]));
        assert_eq!(store.entry(&first).unwrap().id, first);
        assert!(store.branch(Some(&second), 0).is_empty());
        assert_eq!(store.branch(Some(&second), 1).len(), 1);
        assert_eq!(store.branch(Some(&second), 2).len(), 2);
        assert_eq!(store.lanes(), vec!["main"]);
        store
            .append_fact("main", "model", "gpt-test", None)
            .unwrap();
        assert_eq!(
            Reducer::reduce(&store).unwrap().lane("main").unwrap().facts["model"],
            "gpt-test"
        );

        store.append_record(Record::Usage {
            id: "usage-1".into(),
            seq: 4,
            lane: "main".into(),
            timestamp: 4,
            run_id: None,
            cause: UsageCause::Provider,
            entry_id: None,
            tool_call_id: None,
            attempt: None,
            usage: TokenUsage {
                input_tokens: 2,
                total_tokens: 3,
                ..TokenUsage::default()
            },
        });
        assert_eq!(store.lane_log("main", 3, 1).len(), 1);
        assert_eq!(store.usage_sum("main").total_tokens, 3);
    }

    #[test]
    fn model_context_retains_prompt_assistant_and_tool_result_chain() {
        let mut store = MemoryStore::new("session-1");
        let prompt = store.append_message(None, AgentMessage::user("hello", vec![]));
        let assistant = store.append_message(
            Some(prompt.clone()),
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_protocol::RuntimeToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: threadlane_protocol::RuntimeToolCallFunction {
                        name: "load_skill".into(),
                        arguments: r#"{\"name\":\"systematic-debugging\"}"#.into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
        );
        store.append_message(
            Some(assistant),
            AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "load_skill".into(),
                content: "skill instructions".into(),
                is_error: false,
                terminate: false,
            },
        );

        let context = store.model_context("main").unwrap();
        assert_eq!(context.entries.len(), 3);
        assert_eq!(context.entries[0].id, prompt);
        assert!(matches!(
            context.entries[1].message,
            AgentMessage::Assistant { .. }
        ));
        assert!(matches!(
            context.entries[2].message,
            AgentMessage::Tool { .. }
        ));
    }

    #[test]
    fn session_id_generator_is_reload_safe_and_scoped() {
        let generator = SessionIdGenerator::new("session/one");
        let used = vec!["session_one-run-1".into()];
        assert_eq!(generator.next("run", &used), "session_one-run-2");
        assert_eq!(generator.next("run", &[]), "session_one-run-1");
    }
}
