use super::types::{Entry, Record, ReduceError};
use crate::types::TokenUsage;
use std::collections::BTreeSet;

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
    fn session_id_generator_is_reload_safe_and_scoped() {
        let generator = SessionIdGenerator::new("session/one");
        let used = vec!["session_one-run-1".into()];
        assert_eq!(generator.next("run", &used), "session_one-run-2");
        assert_eq!(generator.next("run", &[]), "session_one-run-1");
    }
}
