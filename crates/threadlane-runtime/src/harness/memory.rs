use super::reducer::ReductionContext;
use super::store::SessionStore;
use super::types::{Entry, Record, ReduceError};
#[cfg(test)]
use threadlane_protocol::AgentMessage;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone)]
pub struct MemoryStore {
    session_id: String,
    entries: Vec<Entry>,
    records: Vec<Record>,
    ids: HashSet<String>,
    next_seq: u64,
    /// Streaming reduction state advanced by guard/commit pairs on append,
    /// so validation no longer rebuilds the full context per append (O(n²)
    /// on long sessions). Reads still reduce from scratch.
    reduction: ReductionContext,
}

impl MemoryStore {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            entries: Vec::new(),
            records: Vec::new(),
            ids: HashSet::new(),
            next_seq: 1,
            reduction: ReductionContext::build(&[], &[], BTreeMap::new(), &|_| None)
                .expect("empty reduction builds"),
        }
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }
    fn records(&self) -> &[Record] {
        &self.records
    }

    #[cfg(test)]
    pub(crate) fn append_message(
        &mut self,
        parent_id: Option<String>,
        message: AgentMessage,
    ) -> String {
        self.try_append_message(parent_id, message)
            .expect("valid durable entry")
    }

    #[cfg(test)]
    fn try_append_message(
        &mut self,
        parent_id: Option<String>,
        message: AgentMessage,
    ) -> Result<String, ReduceError> {
        if let Some(parent) = &parent_id {
            if !self.entries.iter().any(|entry| &entry.id == parent) {
                return Err(ReduceError::MissingParent(parent.clone()));
            }
        }
        let entry = Entry {
            id: format!("entry_{}", self.entries.len() + 1),
            parent_id,
            lane: "main".into(),
            seq: self.next_seq,
            timestamp: self.next_seq,
            message,
            surface_op: crate::harness::SurfaceOperation::Append,
            terminate: false,
        };
        let id = entry.id.clone();
        self.try_append_entry(entry)?;
        Ok(id)
    }

    fn try_append_entry(&mut self, entry: Entry) -> Result<(), ReduceError> {
        if entry.id.trim().is_empty() {
            return Err(ReduceError::InvalidRecord("empty entry id".into()));
        }
        if entry.lane.trim().is_empty() {
            return Err(ReduceError::InvalidLane(entry.lane));
        }
        if self.ids.contains(&entry.id) {
            return Err(ReduceError::DuplicateId(entry.id));
        }
        if let Some(parent) = &entry.parent_id {
            if !self.entries.iter().any(|candidate| &candidate.id == parent) {
                return Err(ReduceError::MissingParent(parent.clone()));
            }
        }
        if entry.seq < self.next_seq {
            return Err(ReduceError::NonMonotonicSequence {
                previous: self.next_seq - 1,
                current: entry.seq,
            });
        }
        self.reduction.entry_guard(&entry)?;
        self.ids.insert(entry.id.clone());
        self.next_seq = entry.seq + 1;
        self.entries.push(entry);
        self.reduction
            .commit_entry(self.entries.last().expect("just pushed"));
        Ok(())
    }

    pub fn append_record(&mut self, record: Record) {
        self.try_append_record(record)
            .expect("valid durable record")
    }

    fn try_append_record(&mut self, record: Record) -> Result<(), ReduceError> {
        validate_record(
            &record,
            self.records
                .last()
                .map(Record::seq)
                .into_iter()
                .chain(self.entries.last().map(|entry| entry.seq))
                .max(),
        )?;
        self.reduction.record_guard(&record)?;
        if !self.ids.insert(record.id().to_owned()) {
            return Err(ReduceError::DuplicateId(record.id().to_owned()));
        }
        self.next_seq = record.seq() + 1;
        self.records.push(record);
        self.reduction
            .commit_record(self.records.last().expect("just pushed"));
        Ok(())
    }
}

impl SessionStore for MemoryStore {
    fn session_id(&self) -> &str {
        self.session_id()
    }

    fn entries(&self) -> &[Entry] {
        self.entries()
    }

    fn records(&self) -> &[Record] {
        self.records()
    }

    fn append_entry(&mut self, entry: Entry) -> Result<(), ReduceError> {
        self.try_append_entry(entry)
    }

    fn append_record(&mut self, record: Record) -> Result<(), ReduceError> {
        self.try_append_record(record)
    }
}

fn validate_record(record: &Record, previous: Option<u64>) -> Result<(), ReduceError> {
    if record.id().trim().is_empty() {
        return Err(ReduceError::InvalidRecord("empty record id".into()));
    }
    if record.lane().trim().is_empty() {
        return Err(ReduceError::InvalidLane(record.lane().into()));
    }
    if let Some(previous) = previous {
        if record.seq() <= previous {
            return Err(ReduceError::NonMonotonicSequence {
                previous,
                current: record.seq(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::OperationIntent;

    #[test]
    fn record_sequence_is_checked_against_the_whole_session() {
        let mut store = MemoryStore::new("session");
        store
            .try_append_entry(Entry {
                id: "entry".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 5,
                timestamp: 5,
                message: AgentMessage::user("prompt", vec![]),
                surface_op: crate::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        let error = store
            .try_append_record(Record::OperationStarted {
                id: "run".into(),
                seq: 4,
                lane: "main".into(),
                timestamp: 4,
                source_leaf_id: Some("entry".into()),
                intent: OperationIntent::Run,
            })
            .unwrap_err();
        assert!(matches!(
            error,
            ReduceError::NonMonotonicSequence {
                previous: 5,
                current: 4
            }
        ));
    }

    /// The incremental guard/commit path must project identically to a fresh
    /// full reduction at every step (the JsonlStore invariant, ported here).
    #[test]
    fn incremental_appends_match_full_reduction() {
        use crate::harness::Reducer;
        let mut store = MemoryStore::new("session");
        for index in 1..=8u64 {
            store
                .try_append_entry(Entry {
                    id: format!("entry-{index}"),
                    parent_id: if index == 1 {
                        None
                    } else {
                        Some(format!("entry-{}", index - 1))
                    },
                    lane: "main".into(),
                    seq: index,
                    timestamp: index,
                    message: AgentMessage::user(format!("turn {index}"), vec![]),
                    surface_op: crate::harness::SurfaceOperation::Append,
                    terminate: false,
                })
                .unwrap();
            let incremental = store.reduction.to_reduced_state();
            let fresh = ReductionContext::from_store(&store)
                .unwrap()
                .to_reduced_state();
            assert_eq!(format!("{incremental:?}"), format!("{fresh:?}"));
        }
        let full = Reducer::reduce(&store).unwrap();
        let lane = full.lane("main").unwrap();
        assert_eq!(lane.leaf_id.as_deref(), Some("entry-8"));
    }
}
