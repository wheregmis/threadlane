use super::reducer::{validate_candidate_entry, validate_candidate_record};
use super::store::SessionStore;
use super::types::{Entry, Record, ReduceError};
#[cfg(test)]
use crate::types::AgentMessage;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct MemoryStore {
    session_id: String,
    entries: Vec<Entry>,
    records: Vec<Record>,
    ids: HashSet<String>,
    effects: usize,
    next_seq: u64,
}

impl MemoryStore {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            entries: Vec::new(),
            records: Vec::new(),
            ids: HashSet::new(),
            effects: 0,
            next_seq: 1,
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
    pub fn effect_count(&self) -> usize {
        self.effects
    }

    #[cfg(test)]
    pub(crate) fn append_message(&mut self, parent_id: Option<String>, message: AgentMessage) -> String {
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
        validate_candidate_entry(self, &entry)?;
        self.ids.insert(entry.id.clone());
        self.next_seq = entry.seq + 1;
        self.entries.push(entry);
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
        validate_candidate_record(self, &record)?;
        if !self.ids.insert(record.id().to_owned()) {
            return Err(ReduceError::DuplicateId(record.id().to_owned()));
        }
        self.next_seq = record.seq() + 1;
        self.records.push(record);
        Ok(())
    }

    /// Test-fixture escape hatch for constructing a corrupt durable prefix.
    /// Production callers use the validated `SessionStore` implementation.
    pub fn append_record_unchecked(&mut self, record: Record) {
        validate_record(
            &record,
            self.records
                .last()
                .map(Record::seq)
                .into_iter()
                .chain(self.entries.last().map(|entry| entry.seq))
                .max(),
        )
        .expect("valid record shape");
        if !self.ids.insert(record.id().to_owned()) {
            panic!("duplicate durable record id: {}", record.id());
        }
        self.next_seq = self.next_seq.max(record.seq() + 1);
        self.records.push(record);
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
}
