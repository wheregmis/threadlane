use super::reducer::{validate_candidate_entry, validate_candidate_record};
use super::store::SessionStore;
use super::types::{Entry, Record, ReduceError};
use crate::session_tree::SessionNode;
use crate::types::PlanItem;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashSet as IdSet;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[cfg(unix)]
use std::os::fd::AsRawFd;

#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_NB: i32 = 4;
#[cfg(unix)]
const LOCK_UN: i32 = 8;

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[derive(Debug)]
struct WriterClaim {
    file: Option<fs::File>,
    gate: Mutex<()>,
}

impl Drop for WriterClaim {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            if let Some(file) = &self.file {
                let _ = flock(file.as_raw_fd(), LOCK_UN);
            }
        }
    }
}

fn writer_claim(path: &Path) -> io::Result<Arc<WriterClaim>> {
    static CLAIMS: OnceLock<Mutex<HashMap<PathBuf, Weak<WriterClaim>>>> = OnceLock::new();
    let lock_path = path.with_extension("harness.lock");
    let claims = CLAIMS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut claims = claims
        .lock()
        .map_err(|_| io::Error::other("writer claim registry poisoned"))?;
    if let Some(claim) = claims.get(&lock_path).and_then(Weak::upgrade) {
        return Ok(claim);
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    #[cfg(unix)]
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let claim = Arc::new(WriterClaim {
        file: Some(file),
        gate: Mutex::new(()),
    });
    claims.insert(lock_path, Arc::downgrade(&claim));
    Ok(claim)
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum KnownSessionRecord {
    #[serde(rename = "session_metadata")]
    Metadata {
        name: Option<String>,
        #[serde(default)]
        title_attempted: bool,
        #[serde(default)]
        active_node_id: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    #[serde(rename = "session_plan")]
    Plan {
        #[serde(default)]
        explanation: Option<String>,
        #[serde(default)]
        items: Vec<PlanItem>,
    },
    #[serde(rename = "global_fact")]
    GlobalFact { key: String, value: String },
}

#[derive(Debug, Clone)]
pub struct JsonlStore {
    path: PathBuf,
    claim: Arc<WriterClaim>,
    writable: bool,
    tree: crate::SessionTree,
    entries: Vec<Entry>,
    records: Vec<Record>,
}

impl JsonlStore {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let claim = writer_claim(&path)?;
        Self::load(path, claim, true)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::load(
            path.as_ref().to_path_buf(),
            Arc::new(WriterClaim {
                file: None,
                gate: Mutex::new(()),
            }),
            false,
        )
    }

    fn load(path: PathBuf, claim: Arc<WriterClaim>, writable: bool) -> io::Result<Self> {
        validate_session_lines(&path)?;
        let tree = crate::SessionTree::load_from_file(&path)?;
        let (entries, mut records) = read_entries(&path)?;
        records.extend(read_strict(&path.with_extension("harness.jsonl"))?);
        records.sort_by_key(Record::seq);
        validate_harness_records(&records, &path.with_extension("harness.jsonl"))?;
        let store = Self {
            path,
            claim,
            writable,
            tree,
            entries,
            records,
        };
        super::Reducer::reduce(&store)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        Ok(store)
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        let claim = self.claim.clone();
        let _guard = claim
            .gate
            .lock()
            .map_err(|_| io::Error::other("writer claim poisoned"))?;
        let refreshed = Self::load_parts(&self.path)?;
        self.tree = refreshed.0;
        self.entries = refreshed.1;
        self.records = refreshed.2;
        super::Reducer::reduce(self)
            .map(|_| ())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
    }

    fn load_parts(path: &Path) -> io::Result<(crate::SessionTree, Vec<Entry>, Vec<Record>)> {
        validate_session_lines(path)?;
        let tree = crate::SessionTree::load_from_file(path)?;
        let (entries, mut records) = read_entries(path)?;
        let record_path = path.with_extension("harness.jsonl");
        records.extend(read_strict(&record_path)?);
        records.sort_by_key(Record::seq);
        validate_harness_records(&records, &record_path)?;
        Ok((tree, entries, records))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn tree(&self) -> &crate::SessionTree {
        &self.tree
    }

    pub fn parent_session_id(&self) -> Option<&str> {
        self.tree.parent_session_id.as_deref()
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }
}

impl SessionStore for JsonlStore {
    fn session_id(&self) -> &str {
        &self.tree.session_id
    }
    fn next_sequence(&self) -> u64 {
        self.next_seq()
    }
    fn refresh(&mut self) -> Result<(), ReduceError> {
        JsonlStore::refresh(self).map_err(|error| ReduceError::Storage(error.to_string()))
    }
    fn facts(&self) -> std::collections::BTreeMap<String, String> {
        let mut facts = self
            .tree
            .global_facts
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        for record in &self.records {
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

    fn preferred_leaf(&self, lane: &str) -> Option<String> {
        (lane == "main")
            .then(|| self.tree.active_node_id().map(str::to_owned))
            .flatten()
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn records(&self) -> &[Record] {
        &self.records
    }

    fn append_entry(&mut self, entry: Entry) -> Result<(), ReduceError> {
        if !self.writable {
            return Err(ReduceError::Storage("session store is read-only".into()));
        }
        let claim = self.claim.clone();
        let _guard = claim
            .gate
            .lock()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.reload_unlocked()?;
        if entry.id.trim().is_empty() {
            return Err(ReduceError::InvalidRecord("empty entry id".into()));
        }
        if entry.lane.trim().is_empty() {
            return Err(ReduceError::InvalidLane(entry.lane));
        }
        if self
            .entries
            .iter()
            .any(|candidate| candidate.id == entry.id)
            || self.records.iter().any(|record| record.id() == entry.id)
        {
            return Err(ReduceError::DuplicateId(entry.id));
        }
        if let Some(parent) = &entry.parent_id {
            if !self.entries.iter().any(|candidate| &candidate.id == parent) {
                return Err(ReduceError::MissingParent(parent.clone()));
            }
        }
        if entry.seq < self.next_seq() {
            return Err(ReduceError::NonMonotonicSequence {
                previous: self.next_seq() - 1,
                current: entry.seq,
            });
        }
        validate_candidate_entry(self, &entry)?;
        append_json_line(&self.path, &entry)?;
        self.reload_unlocked()
    }

    fn append_record(&mut self, record: Record) -> Result<(), ReduceError> {
        if !self.writable {
            return Err(ReduceError::Storage("session store is read-only".into()));
        }
        let claim = self.claim.clone();
        let _guard = claim
            .gate
            .lock()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.reload_unlocked()?;
        if record.lane().trim().is_empty() {
            return Err(ReduceError::InvalidLane(record.lane().into()));
        }
        if record.id().trim().is_empty()
            || self.entries.iter().any(|entry| entry.id == record.id())
            || self
                .records
                .iter()
                .any(|current| current.id() == record.id())
        {
            return Err(ReduceError::DuplicateId(record.id().into()));
        }
        if record.seq() < self.next_seq() {
            return Err(ReduceError::NonMonotonicSequence {
                previous: self.next_seq() - 1,
                current: record.seq(),
            });
        }
        validate_candidate_record(self, &record)?;
        append_json_line(&self.path.with_extension("harness.jsonl"), &record)?;
        self.reload_unlocked()
    }
}

impl JsonlStore {
    pub fn append_plan(&mut self, plan: &crate::SessionPlan) -> Result<(), ReduceError> {
        if !self.writable {
            return Err(ReduceError::Storage("session store is read-only".into()));
        }
        let claim = self.claim.clone();
        let _guard = claim
            .gate
            .lock()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        let record = serde_json::json!({
            "type": "session_plan",
            "explanation": plan.explanation,
            "items": plan.items,
        });
        append_json_line(&self.path, &record)?;
        self.reload_unlocked()
    }

    pub fn fork_branch(
        &self,
        path: impl AsRef<Path>,
        session_id: impl Into<String>,
        leaf_id: &str,
    ) -> Result<Self, ReduceError> {
        let mut included = HashSet::new();
        let mut current = Some(leaf_id.to_owned());
        while let Some(id) = current {
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .ok_or_else(|| ReduceError::MissingParent(id.clone()))?;
            included.insert(entry.id.clone());
            current = entry.parent_id.clone();
        }
        self.fork_entries(path, session_id, &included)
    }

    pub fn fork_tree(
        &self,
        path: impl AsRef<Path>,
        session_id: impl Into<String>,
    ) -> Result<Self, ReduceError> {
        let included = self.entries.iter().map(|entry| entry.id.clone()).collect();
        self.fork_entries(path, session_id, &included)
    }

    fn fork_entries(
        &self,
        path: impl AsRef<Path>,
        _session_id: impl Into<String>,
        included: &HashSet<String>,
    ) -> Result<Self, ReduceError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| ReduceError::Storage(error.to_string()))?;
        }
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        let session_id = _session_id.into();
        append_json_line(
            path,
            &serde_json::json!({
                "type": "session_metadata",
                "session_id": session_id,
                "parent_session_id": self.session_id(),
                "name": self.tree.name,
                "model": self.tree.model,
                "active_node_id": null,
                "title_attempted": false
            }),
        )?;
        if self.tree.plan() != &crate::SessionPlan::default() {
            append_json_line(
                path,
                &serde_json::json!({
                    "type": "session_plan",
                    "explanation": self.tree.plan().explanation,
                    "items": self.tree.plan().items,
                }),
            )?;
        }
        let mut fork = Self::open(path).map_err(|error| ReduceError::Storage(error.to_string()))?;
        for source in self
            .entries
            .iter()
            .filter(|entry| included.contains(&entry.id))
        {
            let mut entry = source.clone();
            if entry
                .parent_id
                .as_ref()
                .is_some_and(|parent| !included.contains(parent))
            {
                entry.parent_id = None;
            }
            entry.seq = fork.next_sequence();
            fork.append_entry(entry)?;
        }
        for source in self
            .records
            .iter()
            .filter(|record| matches!(record, Record::FactSet { .. }))
        {
            fork.append_record(source.clone().with_seq(fork.next_sequence()))?;
        }
        for (key, value) in &self.tree.global_facts {
            if !self.records.iter().any(|record| {
                matches!(record, Record::FactSet { key: record_key, .. } if record_key == key)
            }) {
                fork.append_record(Record::FactSet {
                    id: format!("fact-main-{key}"),
                    seq: fork.next_sequence(),
                    lane: "main".into(),
                    timestamp: fork.next_sequence(),
                    run_id: None,
                    key: key.clone(),
                    value: value.clone(),
                })?;
            }
        }
        Ok(fork)
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_seq()
    }

    fn reload_unlocked(&mut self) -> Result<(), ReduceError> {
        let refreshed = Self::load_parts(&self.path)
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.tree = refreshed.0;
        self.entries = refreshed.1;
        self.records = refreshed.2;
        super::Reducer::reduce(self).map(|_| ())
    }

    fn next_seq(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| entry.seq)
            .chain(self.records.iter().map(Record::seq))
            .max()
            .unwrap_or(0)
            + 1
    }
}

fn append_json_line<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), ReduceError> {
    // ponytail: process-wide append lock; the session writer lease handles cross-process writers.
    static APPEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = APPEND_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| ReduceError::Storage(error.to_string()))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| ReduceError::Storage(error.to_string()))?;
    serde_json::to_writer(&mut file, value)
        .map_err(|error| ReduceError::Storage(error.to_string()))?;
    file.write_all(b"\n")
        .and_then(|_| file.sync_all())
        .map_err(|error| ReduceError::Storage(error.to_string()))
}

fn read_entries(path: &Path) -> io::Result<(Vec<Entry>, Vec<Record>)> {
    let values: Vec<serde_json::Value> = read_strict(path)?;
    let mut entries = Vec::new();
    let mut records = Vec::new();
    for (index, value) in values.into_iter().enumerate() {
        if let Ok(record) = serde_json::from_value::<Record>(value.clone()) {
            records.push(record);
            continue;
        }
        if value.get("type").is_some() {
            continue;
        }
        if value.get("seq").is_none() && serde_json::from_value::<Record>(value.clone()).is_ok() {
            continue;
        }
        if value.get("seq").is_some() {
            entries.push(
                serde_json::from_value(value)
                    .map_err(|error| invalid_line(path, index + 1, error))?,
            );
        } else {
            let node: SessionNode = serde_json::from_value(value)
                .map_err(|error| invalid_line(path, index + 1, error))?;
            entries.push(Entry {
                id: node.id,
                parent_id: node.parent_id,
                lane: "main".into(),
                seq: node.seq.unwrap_or((index + 1) as u64),
                timestamp: node.timestamp,
                message: node.message,
                terminate: false,
            });
        }
    }
    Ok((entries, records))
}

fn validate_harness_records(records: &[Record], path: &Path) -> io::Result<()> {
    let mut ids = IdSet::new();
    let mut previous = 0;
    for (index, record) in records.iter().enumerate() {
        if record.id().trim().is_empty() || !ids.insert(record.id().to_owned()) {
            return Err(invalid_line(
                path,
                index + 1,
                "duplicate or empty record id",
            ));
        }
        if record.seq() <= previous {
            return Err(invalid_line(
                path,
                index + 1,
                "non-monotonic record sequence",
            ));
        }
        previous = record.seq();
    }
    Ok(())
}

fn validate_session_lines(path: &Path) -> io::Result<()> {
    let data = fs::read_to_string(path)?;
    for (index, line) in data.split('\n').enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let is_torn_tail = index == data.split('\n').count() - 1 && !data.ends_with('\n');
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_error) if is_torn_tail => break,
            Err(error) => return Err(invalid_line(path, index + 1, error)),
        };
        let result = if serde_json::from_value::<Record>(value.clone()).is_ok() {
            Ok(())
        } else if value.get("lane").is_some() {
            serde_json::from_value::<Entry>(value).map(|_| ())
        } else if value.get("type").is_some() {
            serde_json::from_value::<KnownSessionRecord>(value).map(|_| ())
        } else {
            serde_json::from_value::<SessionNode>(value).map(|_| ())
        };
        if let Err(error) = result {
            return Err(invalid_line(path, index + 1, error));
        }
    }
    Ok(())
}

fn read_strict<T: DeserializeOwned>(path: &Path) -> io::Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(path)?;
    let count = data.split('\n').count();
    let mut values = Vec::new();
    for (index, line) in data.split('\n').enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let is_torn_tail = index == count - 1 && !data.ends_with('\n');
        match serde_json::from_str(line) {
            Ok(value) => values.push(value),
            Err(_error) if is_torn_tail => break,
            Err(error) => return Err(invalid_line(path, index + 1, error)),
        }
    }
    Ok(values)
}

fn invalid_line(path: &Path, line: usize, error: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} line {line}: {error}", path.display()),
    )
}
