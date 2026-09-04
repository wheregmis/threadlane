use super::reducer::ReductionContext;
use super::store::SessionStore;
use super::types::{Entry, Record, ReduceError};
use crate::types::PlanItem;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashSet as IdSet;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[cfg(test)]
thread_local! {
    static LOAD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let lock_path = canonical.with_extension("harness.lock");
    let claims = CLAIMS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut claims = claims
        .lock()
        .map_err(|_| io::Error::other("writer claim registry poisoned"))?;
    if let Some(claim) = claims.get(&lock_path).and_then(Weak::upgrade) {
        return Ok(claim);
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySessionNode {
    id: String,
    parent_id: Option<String>,
    #[serde(default)]
    timestamp: u64,
    #[serde(default)]
    seq: Option<u64>,
    message: crate::types::AgentMessage,
}

#[derive(Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AtomicBatchItem {
    Entry(Entry),
    Record(Record),
}

#[derive(Debug, serde::Serialize, Deserialize)]
struct AtomicBatchLine {
    atomic_batch: Vec<AtomicBatchItem>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SessionLine {
    AtomicBatch(AtomicBatchLine),
    Record(Record),
    Entry(Entry),
    Known(KnownSessionRecord),
    Legacy(LegacySessionNode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptCursor {
    offset: u64,
}

impl TranscriptCursor {
    fn new(offset: u64) -> Self {
        Self { offset }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextCompactedMarker {
    pub seq: u64,
    timestamp: u64,
    pub pre_tokens: usize,
    pub post_tokens: usize,
    pub reason: super::types::CompactionReason,
    effective_model: String,
    context_limit: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    Message(crate::types::AgentMessage),
    ContextCompacted(ContextCompactedMarker),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptPage {
    pub items: Vec<TranscriptItem>,
    pub next_cursor: Option<TranscriptCursor>,
    pub has_older: bool,
}

#[cfg(test)]
impl TranscriptPage {
    fn messages(&self) -> Vec<crate::types::AgentMessage> {
        self.items
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::Message(message) => Some(message.clone()),
                TranscriptItem::ContextCompacted(_) => None,
            })
            .collect()
    }
}

const TRANSCRIPT_READ_CHUNK_BYTES: usize = 64 * 1024;
const ATOMIC_FRAME_SENTINEL: &str = "#!threadlane-atomic-v1 ";
const TORN_EOF_SENTINEL: &str = "#!threadlane-torn-eof-v1";
const LEGACY_ATOMIC_FRAME_PREFIX: &str = "{\"atomic_batch\":[";

/// Reads one chronological page of main-lane transcript items by scanning
/// backward from an opaque byte cursor. This deliberately avoids opening a
/// `JsonlStore`: UI transcript order depends only on durable entry order.
pub fn read_transcript_page(
    path: impl AsRef<Path>,
    cursor: Option<TranscriptCursor>,
    minimum_messages: usize,
) -> io::Result<TranscriptPage> {
    if minimum_messages == 0 {
        return Ok(TranscriptPage {
            items: Vec::new(),
            next_cursor: cursor,
            has_older: cursor.is_some(),
        });
    }
    let path = path.as_ref();
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(TranscriptPage {
                items: Vec::new(),
                next_cursor: None,
                has_older: false,
            });
        }
        Err(error) => return Err(error),
    };
    let file_len = file.metadata()?.len();
    let requested_end = cursor.map_or(file_len, |cursor| cursor.offset.min(file_len));
    let at_eof = cursor.is_none_or(|cursor| cursor.offset >= file_len);
    let mut scan_end = if at_eof {
        complete_jsonl_end(&mut file, path, requested_end)?
    } else {
        requested_end
    };
    let mut suffix = Vec::new();
    let mut items = Vec::new();
    let mut message_count = 0;
    let mut next_offset = 0;

    'chunks: while scan_end > 0 {
        let chunk_start = scan_end.saturating_sub(TRANSCRIPT_READ_CHUNK_BYTES as u64);
        let mut chunk = vec![0; (scan_end - chunk_start) as usize];
        file.seek(SeekFrom::Start(chunk_start))?;
        file.read_exact(&mut chunk)?;
        let mut segment_end = chunk.len();

        for newline in chunk
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
        {
            let mut line = chunk[newline + 1..segment_end].to_vec();
            if !suffix.is_empty() {
                line.extend_from_slice(&suffix);
                suffix.clear();
            }
            segment_end = newline;
            if line.is_empty() {
                continue;
            }
            let line_start = chunk_start + newline as u64 + 1;
            next_offset = line_start.saturating_sub(1);
            let line_items = transcript_items(path, line_start, &line)?;
            // A journal line is the smallest cursor-addressable unit. Expand an
            // atomic frame completely so no visible sibling is stranded.
            let mut stop_after_line = false;
            for item in line_items.into_iter().rev() {
                let starts_turn =
                    matches!(&item, TranscriptItem::Message(message) if message.is_user());
                if matches!(item, TranscriptItem::Message(_)) {
                    message_count += 1;
                }
                items.push(item);
                stop_after_line |= message_count >= minimum_messages && starts_turn;
            }
            if stop_after_line {
                break 'chunks;
            }
        }

        let mut prefix = chunk[..segment_end].to_vec();
        prefix.extend_from_slice(&suffix);
        suffix = prefix;
        scan_end = chunk_start;
    }

    if scan_end == 0 && !suffix.is_empty() {
        next_offset = 0;
        items.extend(transcript_items(path, 0, &suffix)?.into_iter().rev());
    }

    items.reverse();
    let next_cursor = (next_offset > 0).then(|| TranscriptCursor::new(next_offset));
    Ok(TranscriptPage {
        items,
        next_cursor,
        has_older: next_cursor.is_some(),
    })
}

fn complete_jsonl_end(file: &mut fs::File, path: &Path, end: u64) -> io::Result<u64> {
    if end == 0 {
        return Ok(0);
    }
    file.seek(SeekFrom::Start(end - 1))?;
    let mut final_byte = [0];
    file.read_exact(&mut final_byte)?;
    if final_byte[0] == b'\n' {
        return Ok(end);
    }
    let mut scan_end = end;
    let tail_start = loop {
        let start = scan_end.saturating_sub(TRANSCRIPT_READ_CHUNK_BYTES as u64);
        let mut chunk = vec![0; (scan_end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk)?;
        if let Some(newline) = chunk.iter().rposition(|byte| *byte == b'\n') {
            break start + newline as u64 + 1;
        }
        if start == 0 {
            break 0;
        }
        scan_end = start;
    };
    let mut tail = vec![0; (end - tail_start) as usize];
    file.seek(SeekFrom::Start(tail_start))?;
    file.read_exact(&mut tail)?;
    if parse_transcript_line(path, tail_start, &tail, true)?.is_some() {
        Ok(end)
    } else {
        Ok(tail_start)
    }
}

fn parse_transcript_line(
    path: &Path,
    offset: u64,
    bytes: &[u8],
    is_physical_eof: bool,
) -> io::Result<Option<SessionLine>> {
    let payload = atomic_frame_payload(bytes).unwrap_or(bytes);
    match serde_json::from_slice(payload) {
        Ok(line) => Ok(Some(line)),
        Err(_) if is_recoverable_atomic_fragment(bytes, is_physical_eof) => Ok(None),
        Err(error) => Err(invalid_line(path, offset as usize, error)),
    }
}
fn transcript_items(path: &Path, offset: u64, bytes: &[u8]) -> io::Result<Vec<TranscriptItem>> {
    let Some(line) = parse_transcript_line(path, offset, bytes, false)? else {
        return Ok(Vec::new());
    };
    Ok(match line {
        SessionLine::AtomicBatch(batch) => batch
            .atomic_batch
            .into_iter()
            .filter_map(|item| match item {
                AtomicBatchItem::Entry(entry)
                    if entry.lane == "main"
                        && matches!(entry.surface_op, super::types::SurfaceOperation::Append) =>
                {
                    match entry.message {
                        crate::types::AgentMessage::Custom {
                            ref custom_type, ..
                        } if custom_type == "compaction_summary" => None,
                        message => Some(TranscriptItem::Message(message)),
                    }
                }
                AtomicBatchItem::Record(Record::ContextCompacted {
                    seq,
                    lane,
                    timestamp,
                    pre_tokens,
                    post_tokens,
                    reason,
                    effective_model,
                    context_limit,
                    ..
                }) if lane == "main" => {
                    Some(TranscriptItem::ContextCompacted(ContextCompactedMarker {
                        seq,
                        timestamp,
                        pre_tokens,
                        post_tokens,
                        reason,
                        effective_model: effective_model.as_str().to_owned(),
                        context_limit,
                    }))
                }
                _ => None,
            })
            .collect(),
        SessionLine::Entry(entry)
            if entry.lane == "main"
                && matches!(entry.surface_op, super::types::SurfaceOperation::Append) =>
        {
            match entry.message {
                crate::types::AgentMessage::Custom {
                    ref custom_type, ..
                } if custom_type == "compaction_summary" => Vec::new(),
                message => vec![TranscriptItem::Message(message)],
            }
        }
        SessionLine::Legacy(node) => vec![TranscriptItem::Message(node.message)],
        SessionLine::Record(Record::ContextCompacted {
            seq,
            lane,
            timestamp,
            pre_tokens,
            post_tokens,
            reason,
            effective_model,
            context_limit,
            ..
        }) if lane == "main" => vec![TranscriptItem::ContextCompacted(ContextCompactedMarker {
            seq,
            timestamp,
            pre_tokens,
            post_tokens,
            reason,
            effective_model: effective_model.as_str().to_owned(),
            context_limit,
        })],
        _ => Vec::new(),
    })
}

#[derive(Debug, Clone)]
pub struct JsonlStore {
    path: PathBuf,
    session_id: String,
    claim: Arc<WriterClaim>,
    writable: bool,
    entries: Vec<Entry>,
    records: Vec<Record>,
    preferred_leaf: Option<String>,
    session_file_len: u64,
    harness_file_len: u64,
    /// Highest sequence across entries and records, maintained incrementally
    /// so sequence allocation does not rescan the whole file.
    max_seq: u64,
    /// Entry ids only (valid parents for new entries), maintained
    /// incrementally for O(1) parent and duplicate checks.
    entry_ids: HashSet<String>,
    /// Record ids, maintained incrementally for O(1) duplicate checks.
    record_ids: HashSet<String>,
    /// Streaming reduction state advanced by guard/commit pairs on append;
    /// rebuilt wholesale whenever the file is reloaded.
    reduction: ReductionContext,
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
        #[cfg(test)]
        LOAD_COUNT.with(|count| count.set(count.get() + 1));
        let _guard = claim
            .gate
            .lock()
            .map_err(|_| io::Error::other("writer claim poisoned"))?;
        let (session_id, preferred_leaf, entries, records) = Self::load_parts(&path)?;
        let session_file_len = file_len(&path)?;
        let harness_file_len = file_len(&path.with_extension("harness.jsonl"))?;
        // Mirrors SessionStore::facts over the freshly parsed record stream.
        let mut fact_seed = std::collections::BTreeMap::new();
        for record in &records {
            if let Record::FactSet {
                key,
                value,
                run_id: None,
                ..
            } = record
            {
                fact_seed.insert(key.clone(), value.clone());
            }
        }
        let preferred_main = preferred_leaf.clone();
        let reduction = ReductionContext::build(&entries, &records, fact_seed, &|lane: &str| {
            (lane == "main").then(|| preferred_main.clone()).flatten()
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let max_seq = entries
            .iter()
            .map(|entry| entry.seq)
            .chain(records.iter().map(Record::seq))
            .max()
            .unwrap_or(0);
        let entry_ids = entries.iter().map(|entry| entry.id.clone()).collect();
        let record_ids = records
            .iter()
            .map(|record| record.id().to_owned())
            .collect();
        Ok(Self {
            path,
            session_id,
            claim: claim.clone(),
            writable,
            entries,
            records,
            preferred_leaf,
            session_file_len,
            harness_file_len,
            max_seq,
            entry_ids,
            record_ids,
            reduction,
        })
    }

    /// Recomputes everything derived from the entry/record streams after a
    /// wholesale reload: the incremental indexes and the streaming reduction
    /// context. Validation happens inside the context build, so failures
    /// surface the same reduce errors as before.
    fn rebuild_derived_state(&mut self) -> io::Result<()> {
        self.reduction =
            ReductionContext::build(&self.entries, &self.records, self.facts(), &|lane| {
                self.preferred_leaf(lane)
            })
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        self.max_seq = self
            .entries
            .iter()
            .map(|entry| entry.seq)
            .chain(self.records.iter().map(Record::seq))
            .max()
            .unwrap_or(0);
        self.entry_ids = self.entries.iter().map(|entry| entry.id.clone()).collect();
        self.record_ids = self
            .records
            .iter()
            .map(|record| record.id().to_owned())
            .collect();
        Ok(())
    }

    fn refresh(&mut self) -> io::Result<()> {
        let claim = self.claim.clone();
        let _guard = claim
            .gate
            .lock()
            .map_err(|_| io::Error::other("writer claim poisoned"))?;
        // reload_unlocked rebuilds the streaming reduction context itself;
        // a second full reduce here would be redundant.
        self.reload_unlocked()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
    }

    /// Reloads from disk only when another writer has appended (detected via
    /// the cheap file-length check), otherwise leaves parsed state intact.
    pub fn ensure_fresh(&mut self) -> Result<(), ReduceError> {
        if !self
            .is_fresh()
            .map_err(|error| ReduceError::Storage(error.to_string()))?
        {
            let claim = self.claim.clone();
            let _guard = claim
                .gate
                .lock()
                .map_err(|error| ReduceError::Storage(error.to_string()))?;
            if !self
                .is_fresh()
                .map_err(|error| ReduceError::Storage(error.to_string()))?
            {
                return self.reload_unlocked();
            }
        }
        Ok(())
    }

    fn refresh_file_lengths(&mut self) -> io::Result<()> {
        self.session_file_len = file_len(&self.path)?;
        self.harness_file_len = file_len(&self.path.with_extension("harness.jsonl"))?;
        Ok(())
    }

    fn is_fresh(&self) -> io::Result<bool> {
        Ok(self.session_file_len == file_len(&self.path)?
            && self.harness_file_len == file_len(&self.path.with_extension("harness.jsonl"))?)
    }

    fn load_parts(path: &Path) -> io::Result<(String, Option<String>, Vec<Entry>, Vec<Record>)> {
        let lines = read_strict::<SessionLine>(path)?;
        let (session_id, preferred_leaf, entries, mut records) = classify_lines(path, lines);
        let record_path = path.with_extension("harness.jsonl");
        records.extend(read_strict(&record_path)?);
        records.sort_by_key(Record::seq);
        validate_harness_records(&records, path)?;
        Ok((session_id, preferred_leaf, entries, records))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Projects the streaming reduction context without rebuilding it;
    /// used to assert equivalence against a fresh full reduction in tests.
    fn reduced_state(&self) -> super::types::ReducedState {
        self.reduction.to_reduced_state()
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
        &self.session_id
    }
    fn reduced_state(&self) -> Option<super::types::ReducedState> {
        Some(self.reduced_state())
    }
    fn next_sequence(&self) -> u64 {
        self.next_seq()
    }
    fn refresh(&mut self) -> Result<(), ReduceError> {
        JsonlStore::refresh(self).map_err(|error| ReduceError::Storage(error.to_string()))
    }
    fn facts(&self) -> std::collections::BTreeMap<String, String> {
        let mut facts = std::collections::BTreeMap::new();
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
            .then(|| self.preferred_leaf.clone())
            .flatten()
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn records(&self) -> &[Record] {
        &self.records
    }

    fn append_actions_atomically(
        &mut self,
        actions: &[super::EffectAction],
    ) -> Result<(), ReduceError> {
        if !self.writable {
            return Err(ReduceError::Storage("session store is read-only".into()));
        }
        let claim = self.claim.clone();
        let _guard = claim
            .gate
            .lock()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        if !self
            .is_fresh()
            .map_err(|error| ReduceError::Storage(error.to_string()))?
        {
            self.reload_unlocked()?;
        }

        // Validate and sequence the complete group against a disposable reducer.
        // Nothing reaches disk or observable in-memory state until every logical
        // checkpoint/tail/telemetry boundary has passed validation.
        let mut reduction = self.reduction.clone();
        let mut next_seq = self.next_seq();
        let mut items = Vec::with_capacity(actions.len());
        let final_main_leaf = actions.iter().rev().find_map(|action| match action {
            super::EffectAction::AppendEntry { entry } if entry.lane == "main" => {
                Some(entry.id.clone())
            }
            _ => None,
        });
        // Entries establish the complete parent chain before the lane move makes
        // that chain canonical; operational/telemetry records follow it.
        for action in actions
            .iter()
            .filter(|action| matches!(action, super::EffectAction::AppendEntry { .. }))
            .chain(
                actions
                    .iter()
                    .filter(|action| matches!(action, super::EffectAction::AppendRecord { .. })),
            )
        {
            match action {
                super::EffectAction::AppendEntry { entry } => {
                    let mut entry = entry.clone();
                    entry.seq = next_seq;
                    next_seq += 1;
                    reduction.entry_guard(&entry)?;
                    if entry.lane == "main" {
                        reduction.set_preferred_leaf_main(entry.id.clone());
                    }
                    reduction.commit_entry(&entry);
                    items.push(AtomicBatchItem::Entry(entry));
                }
                super::EffectAction::AppendRecord { record, .. } => {
                    let mut record = record.clone().with_seq(next_seq);
                    if let (Record::LaneMoved { target_leaf_id, .. }, Some(leaf)) =
                        (&mut record, &final_main_leaf)
                    {
                        *target_leaf_id = leaf.clone();
                    }
                    next_seq += 1;
                    reduction.record_guard(&record)?;
                    reduction.commit_record(&record);
                    items.push(AtomicBatchItem::Record(record));
                }
            }
        }

        // A batch is serialized as one JSONL frame and synced before success is
        // reported. The leading newline is intentional: after a crash leaves a
        // partial final frame, the next append quarantines that frame instead of
        // concatenating onto it. Recovery recognizes only that reserved torn
        // atomic-frame prefix; existing canonical bytes are never truncated or
        // rewritten.
        append_atomic_batch_line(
            &self.path,
            &AtomicBatchLine {
                atomic_batch: items,
            },
        )?;
        self.reload_unlocked()?;
        Ok(())
    }

    fn append_entry(&mut self, mut entry: Entry) -> Result<(), ReduceError> {
        if !self.writable {
            return Err(ReduceError::Storage("session store is read-only".into()));
        }
        let claim = self.claim.clone();
        let _guard = claim
            .gate
            .lock()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        if !self
            .is_fresh()
            .map_err(|error| ReduceError::Storage(error.to_string()))?
        {
            self.reload_unlocked()?;
        }
        // Sequence allocation belongs to the writer, not callers. This is the
        // only point where a sequence becomes durable. Validation runs before
        // the preferred-leaf update so the dangling-pointer check judges the
        // previously committed leaf; the commit then observes the new one,
        // matching what a fresh full reduce would read from the store.
        entry.seq = self.next_seq();
        self.reduction.entry_guard(&entry)?;
        append_json_line(&self.path, &entry, SyncPolicy::All)?;
        self.session_file_len =
            file_len(&self.path).map_err(|error| ReduceError::Storage(error.to_string()))?;
        if entry.lane == "main" {
            let leaf = entry.id.clone();
            self.preferred_leaf = Some(leaf.clone());
            self.reduction.set_preferred_leaf_main(leaf);
        }
        self.reduction.commit_entry(&entry);
        self.max_seq = entry.seq;
        self.entry_ids.insert(entry.id.clone());
        self.entries.push(entry);
        Ok(())
    }

    fn append_record(&mut self, mut record: Record) -> Result<(), ReduceError> {
        if !self.writable {
            return Err(ReduceError::Storage("session store is read-only".into()));
        }
        let claim = self.claim.clone();
        let _guard = claim
            .gate
            .lock()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        if !self
            .is_fresh()
            .map_err(|error| ReduceError::Storage(error.to_string()))?
        {
            self.reload_unlocked()?;
        }
        // Identity checks precede sequence allocation; stateful guards run on
        // the allocated sequence, matching the historical validate-candidate
        // ordering without cloning history.
        if record.lane().trim().is_empty() {
            return Err(ReduceError::InvalidLane(record.lane().into()));
        }
        let record_id = record.id();
        if record_id.trim().is_empty()
            || self.entry_ids.contains(record_id)
            || self.record_ids.contains(record_id)
        {
            return Err(ReduceError::DuplicateId(record_id.into()));
        }
        record = record.with_seq(self.next_seq());
        self.reduction.record_guard(&record)?;
        append_json_line(&self.path, &record, record.sync_policy())?;
        self.session_file_len =
            file_len(&self.path).map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.reduction.commit_record(&record);
        self.max_seq = record.seq();
        self.record_ids.insert(record.id().to_owned());
        self.records.push(record);
        Ok(())
    }
}

impl JsonlStore {
    pub fn append_plan(&mut self, plan: &crate::SessionPlan) -> Result<(), ReduceError> {
        let record = Record::FactSet {
            id: format!("fact-plan-{}", self.next_sequence()),
            seq: self.next_sequence(),
            lane: "main".into(),
            timestamp: 0,
            run_id: None,
            key: "session_plan".into(),
            value: serde_json::to_string(plan)
                .map_err(|e| ReduceError::InvalidRecord(e.to_string()))?,
        };
        self.append_record(record)
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
        let _session_id = _session_id.into();
        let mut fork = Self::open(path).map_err(|error| ReduceError::Storage(error.to_string()))?;
        fork.append_record(Record::FactSet {
            id: "fact-main-parent_session_id".into(),
            seq: fork.next_sequence(),
            lane: "main".into(),
            timestamp: 0,
            run_id: None,
            key: "parent_session_id".into(),
            value: self.session_id().to_string(),
        })?;
        if let Some(model) = self.model() {
            fork.append_record(Record::FactSet {
                id: "fact-main-model".into(),
                seq: fork.next_sequence(),
                lane: "main".into(),
                timestamp: 0,
                run_id: None,
                key: "model".into(),
                value: model,
            })?;
        }
        if let Some(name) = self.name() {
            fork.append_record(Record::FactSet {
                id: "fact-main-name".into(),
                seq: fork.next_sequence(),
                lane: "main".into(),
                timestamp: 0,
                run_id: None,
                key: "name".into(),
                value: name,
            })?;
        }
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
        for (key, value) in self.facts() {
            if !fork.records.iter().any(|record| {
                matches!(record, Record::FactSet { key: record_key, .. } if record_key == &key)
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

    /// Reloads in-memory state from disk when another writer has appended.
    /// Caller must hold the claim gate.
    fn reload_unlocked(&mut self) -> Result<(), ReduceError> {
        let refreshed = Self::load_parts(&self.path)
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.session_id = refreshed.0;
        self.preferred_leaf = refreshed.1;
        self.entries = refreshed.2;
        self.records = refreshed.3;
        self.refresh_file_lengths()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.rebuild_derived_state()
            .map_err(|error| ReduceError::Storage(error.to_string()))
    }

    fn next_seq(&self) -> u64 {
        self.max_seq + 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncPolicy {
    All,
    Data,
}

impl Record {
    fn sync_policy(&self) -> SyncPolicy {
        match self {
            Self::ContextManifestCaptured { .. }
            | Self::ContextSnapshotIndexed { .. }
            | Self::ContextSnapshotLoaded { .. }
            | Self::ContextCompacted { .. }
            | Self::RunContextCaptured { .. }
            | Self::ProviderRequestStarted { .. }
            | Self::ProviderRequestFinished { .. }
            | Self::ProviderResponseAttached { .. }
            | Self::StreamCheckpoint { .. } => SyncPolicy::Data,
            Self::OperationStarted { .. }
            | Self::AbortRequested { .. }
            | Self::OperationFinished { .. }
            | Self::LaneMoved { .. }
            | Self::StepAttempt { .. }
            | Self::RetryScheduled { .. }
            | Self::RetryConsumed { .. }
            | Self::ToolStarted { .. }
            | Self::ToolFinished { .. }
            | Self::QueueEnqueued { .. }
            | Self::QueueCancelled { .. }
            | Self::QueueConsumed { .. }
            | Self::WriteDeferred { .. }
            | Self::WriteApplied { .. }
            | Self::FactSet { .. }
            | Self::HookResumeData { .. }
            | Self::Usage { .. }
            | Self::PermissionRequested { .. }
            | Self::PermissionResolved { .. }
            | Self::ToolExecutionObserved { .. }
            | Self::AbortObserved { .. }
            | Self::SubagentLifecycle { .. } => SyncPolicy::All,
        }
    }
}

fn append_session_json_line_with_policy<T: serde::Serialize>(
    path: &Path,
    value: &T,
    sync_policy: SyncPolicy,
) -> io::Result<()> {
    // Process-wide append lock; the session writer lease handles cross-process writers.
    static APPEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = APPEND_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    // The separator also quarantines a crash-torn atomic batch if ordinary
    // session traffic is the first append after recovery.
    prepare_append_boundary(&mut file)?;
    serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    match sync_policy {
        SyncPolicy::All => file.sync_all(),
        SyncPolicy::Data => file.sync_data(),
    }
}

fn prepare_append_boundary(file: &mut fs::File) -> io::Result<()> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut data = Vec::with_capacity(len as usize);
    file.read_to_end(&mut data)?;
    if data.last() == Some(&b'\n') {
        return Ok(());
    }
    let tail = data.rsplit(|byte| *byte == b'\n').next().unwrap_or(&data);
    let payload = atomic_frame_payload(tail).unwrap_or(tail);
    if serde_json::from_slice::<serde_json::Value>(payload).is_err()
        && is_atomic_frame_fragment(tail)
    {
        file.write_all(TORN_EOF_SENTINEL.as_bytes())?;
    }
    file.write_all(b"\n")
}

fn file_len(path: &Path) -> io::Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn append_json_line<T: serde::Serialize>(
    path: &Path,
    value: &T,
    sync_policy: SyncPolicy,
) -> Result<(), ReduceError> {
    append_session_json_line_with_policy(path, value, sync_policy)
        .map_err(|error| ReduceError::Storage(error.to_string()))
}

fn append_atomic_batch_line(path: &Path, value: &AtomicBatchLine) -> Result<(), ReduceError> {
    let encoded =
        serde_json::to_vec(value).map_err(|error| ReduceError::Storage(error.to_string()))?;
    static APPEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = APPEND_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| ReduceError::Storage(error.to_string()))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|error| ReduceError::Storage(error.to_string()))?;
    prepare_append_boundary(&mut file)
        .and_then(|_| file.write_all(ATOMIC_FRAME_SENTINEL.as_bytes()))
        .and_then(|_| file.write_all(&encoded))
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .map_err(|error| ReduceError::Storage(error.to_string()))
}

/// Typed classification entry point retained for focused compatibility tests.
#[cfg(test)]
fn read_entries(path: &Path) -> io::Result<(String, Option<String>, Vec<Entry>, Vec<Record>)> {
    let lines = read_strict::<SessionLine>(path)?;
    Ok(classify_lines(path, lines))
}

fn classify_lines(
    path: &Path,
    lines: Vec<SessionLine>,
) -> (String, Option<String>, Vec<Entry>, Vec<Record>) {
    let session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "session".into());
    let mut preferred_leaf = None;
    let mut entries = Vec::new();
    let mut records = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        match line {
            SessionLine::AtomicBatch(batch) => {
                for item in batch.atomic_batch {
                    match item {
                        AtomicBatchItem::Entry(entry) => {
                            if entry.lane == "main" {
                                preferred_leaf = Some(entry.id.clone());
                            }
                            entries.push(entry);
                        }
                        AtomicBatchItem::Record(record) => records.push(record),
                    }
                }
            }
            SessionLine::Record(record) => records.push(record),
            SessionLine::Entry(entry) => {
                if entry.lane == "main" {
                    preferred_leaf = Some(entry.id.clone());
                }
                entries.push(entry);
            }
            SessionLine::Known(known) => match known {
                KnownSessionRecord::Metadata {
                    name,
                    title_attempted,
                    active_node_id,
                    model,
                } => {
                    if let Some(active) = active_node_id {
                        preferred_leaf = Some(active);
                    }
                    if let Some(name) = name {
                        records.push(Record::FactSet {
                            id: format!("fact-name-{}", index + 1),
                            seq: 0,
                            lane: "main".into(),
                            timestamp: 0,
                            run_id: None,
                            key: "name".into(),
                            value: name,
                        });
                    }
                    if let Some(model) = model {
                        records.push(Record::FactSet {
                            id: format!("fact-model-{}", index + 1),
                            seq: 0,
                            lane: "main".into(),
                            timestamp: 0,
                            run_id: None,
                            key: "model".into(),
                            value: model,
                        });
                    }
                    if title_attempted {
                        records.push(Record::FactSet {
                            id: format!("fact-title-attempted-{}", index + 1),
                            seq: 0,
                            lane: "main".into(),
                            timestamp: 0,
                            run_id: None,
                            key: "title_attempted".into(),
                            value: "true".into(),
                        });
                    }
                }
                KnownSessionRecord::Plan { items, explanation } => {
                    let plan = crate::types::SessionPlan { explanation, items };
                    if let Ok(plan_json) = serde_json::to_string(&plan) {
                        records.push(Record::FactSet {
                            id: format!("fact-plan-{}", index + 1),
                            seq: 0,
                            lane: "main".into(),
                            timestamp: 0,
                            run_id: None,
                            key: "session_plan".into(),
                            value: plan_json,
                        });
                    }
                }
                KnownSessionRecord::GlobalFact { key, value } => {
                    records.push(Record::FactSet {
                        id: format!("fact-{key}-{}", index + 1),
                        seq: 0,
                        lane: "main".into(),
                        timestamp: 0,
                        run_id: None,
                        key,
                        value,
                    });
                }
            },
            SessionLine::Legacy(node) => {
                preferred_leaf = Some(node.id.clone());
                entries.push(Entry {
                    id: node.id,
                    parent_id: node.parent_id,
                    lane: "main".into(),
                    seq: node.seq.unwrap_or((index + 1) as u64),
                    timestamp: node.timestamp,
                    message: node.message,
                    surface_op: super::types::SurfaceOperation::Append,
                    terminate: false,
                });
            }
        }
    }
    // Legacy metadata has no harness sequence. Allocate virtual values after
    // the durable stream so it cannot collide with a V2 record sequence.
    let mut next_seq = entries
        .iter()
        .map(|entry| entry.seq)
        .chain(records.iter().map(Record::seq))
        .max()
        .unwrap_or(0)
        + 1;
    for record in &mut records {
        if record.seq() == 0 {
            *record = record.clone().with_seq(next_seq);
            next_seq += 1;
        }
    }
    (session_id, preferred_leaf, entries, records)
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

fn atomic_frame_payload(bytes: &[u8]) -> Option<&[u8]> {
    bytes.strip_prefix(ATOMIC_FRAME_SENTINEL.as_bytes())
}

fn is_atomic_frame_fragment(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && (ATOMIC_FRAME_SENTINEL.as_bytes().starts_with(bytes)
            || bytes.starts_with(ATOMIC_FRAME_SENTINEL.as_bytes())
            || LEGACY_ATOMIC_FRAME_PREFIX.as_bytes().starts_with(bytes)
            || bytes.starts_with(LEGACY_ATOMIC_FRAME_PREFIX.as_bytes()))
}

fn is_recoverable_atomic_fragment(bytes: &[u8], is_physical_eof: bool) -> bool {
    (is_physical_eof && is_atomic_frame_fragment(bytes))
        || bytes
            .strip_suffix(TORN_EOF_SENTINEL.as_bytes())
            .is_some_and(is_atomic_frame_fragment)
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
        let is_physical_eof = index == count - 1 && !data.ends_with('\n');
        let payload = line.strip_prefix(ATOMIC_FRAME_SENTINEL).unwrap_or(line);
        match serde_json::from_str(payload) {
            Ok(value) => values.push(value),
            Err(_error) if is_recoverable_atomic_fragment(line.as_bytes(), is_physical_eof) => {
                continue
            }
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

#[cfg(test)]
mod tests {
    use super::{
        append_atomic_batch_line, read_entries, read_transcript_page, AtomicBatchItem,
        AtomicBatchLine, SyncPolicy, TranscriptCursor, TranscriptItem, ATOMIC_FRAME_SENTINEL,
        LEGACY_ATOMIC_FRAME_PREFIX, TORN_EOF_SENTINEL,
    };
    use crate::harness::{
        AgentHarness, CompactionReason, ContextItemSource, ContextItemStatus, ContextManifestItem,
        ContextSnapshot, ContextSnapshotLoadOutcome, HarnessEventHub, JsonlStore, Record, Reducer,
        SessionStore, TraceString,
    };
    use crate::types::AgentMessage;

    fn user_entry(id: &str, lane: &str) -> crate::harness::Entry {
        crate::harness::Entry::new(
            id,
            None,
            lane,
            0,
            0,
            AgentMessage::User {
                content: "hello".into(),
            },
            false,
        )
    }

    fn transcript_entry(index: usize, message: AgentMessage) -> crate::harness::Entry {
        crate::harness::Entry::new(
            format!("entry-{index}"),
            None,
            "main",
            index as u64 + 1,
            index as u64 + 1,
            message,
            false,
        )
    }

    fn assistant(content: impl Into<String>) -> AgentMessage {
        AgentMessage::Assistant {
            content: Some(content.into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        }
    }

    fn context_snapshot() -> ContextSnapshot {
        ContextSnapshot {
            context_id: "ctx-v2-tool-result-call-1".into(),
            source_lane: "main".into(),
            source_run_id: "run-1".into(),
            source_tool_call_id: "call-1".into(),
            source_entry_id: "v2-tool-result-call-1".into(),
            path: "src/lib.rs".into(),
            start_line: Some(10),
            end_line: Some(20),
            file_sha256: TraceString::new("a".repeat(64)).unwrap(),
            output_chars: 123,
            captured_at: 1,
        }
    }

    #[test]
    fn context_snapshot_records_reload_without_changing_model_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context-snapshot.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store
            .append_entry(crate::harness::Entry::new(
                "v2-tool-result-call-1",
                None,
                "main",
                1,
                1,
                AgentMessage::Tool {
                    tool_call_id: "call-1".into(),
                    name: "read_file".into(),
                    content: "contents".into(),
                    is_error: false,
                    terminate: false,
                },
                false,
            ))
            .unwrap();
        let model_context = store.model_context("main").unwrap();
        let snapshot = context_snapshot();
        store
            .append_record(Record::ContextSnapshotIndexed {
                id: "snapshot-index-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: "run-1".into(),
                snapshot: snapshot.clone(),
            })
            .unwrap();
        assert_eq!(
            store
                .append_record(Record::ContextSnapshotIndexed {
                    id: "snapshot-index-1".into(),
                    seq: store.next_sequence(),
                    lane: "main".into(),
                    timestamp: 1,
                    run_id: "run-1".into(),
                    snapshot: snapshot.clone(),
                })
                .unwrap_err(),
            crate::harness::ReduceError::DuplicateId("snapshot-index-1".into())
        );
        store
            .append_record(Record::ContextSnapshotLoaded {
                id: "snapshot-load-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: "run-1".into(),
                context_id: snapshot.context_id.clone(),
                source_lane: snapshot.source_lane.clone(),
                current_digest: None,
                outcome: ContextSnapshotLoadOutcome::Loaded,
            })
            .unwrap();
        drop(store);

        let reloaded = JsonlStore::open(&path).unwrap();
        let state = Reducer::reduce(&reloaded).unwrap();
        assert_eq!(
            state.lane("main").unwrap().context_snapshots,
            vec![snapshot]
        );
        assert_eq!(reloaded.model_context("main").unwrap(), model_context);
    }

    #[test]
    fn malformed_context_snapshot_metadata_is_skipped_during_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("malformed-context-snapshot.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store
            .append_entry(crate::harness::Entry::new(
                "v2-tool-result-call-1",
                None,
                "main",
                1,
                1,
                AgentMessage::Tool {
                    tool_call_id: "call-1".into(),
                    name: "read_file".into(),
                    content: "contents".into(),
                    is_error: false,
                    terminate: false,
                },
                false,
            ))
            .unwrap();
        drop(store);
        let malformed = Record::ContextSnapshotIndexed {
            id: "snapshot-index-malformed".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 1,
            run_id: "run-1".into(),
            snapshot: ContextSnapshot {
                file_sha256: TraceString::new("not-a-digest").unwrap(),
                ..context_snapshot()
            },
        };
        use std::io::Write as _;
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap(),
            "{}",
            serde_json::to_string(&malformed).unwrap()
        )
        .unwrap();
        let malformed_load = Record::ContextSnapshotLoaded {
            id: "snapshot-load-malformed".into(),
            seq: 3,
            lane: "main".into(),
            timestamp: 1,
            run_id: "run-1".into(),
            context_id: String::new(),
            source_lane: "main".into(),
            current_digest: None,
            outcome: ContextSnapshotLoadOutcome::Corrupt,
        };
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap(),
            "{}",
            serde_json::to_string(&malformed_load).unwrap()
        )
        .unwrap();

        let reloaded = JsonlStore::open(&path).unwrap();
        assert!(Reducer::reduce(&reloaded)
            .unwrap()
            .lane("main")
            .unwrap()
            .context_snapshots
            .is_empty());
    }

    fn write_transcript(path: &std::path::Path, messages: Vec<AgentMessage>) {
        let data = messages
            .into_iter()
            .enumerate()
            .map(|(index, message)| {
                serde_json::to_string(&transcript_entry(index, message)).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, format!("{data}\n")).unwrap();
    }

    #[test]
    fn transcript_page_reads_newest_turn_then_continues_backward() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        write_transcript(
            &path,
            (0..6)
                .flat_map(|turn| {
                    [
                        AgentMessage::user(format!("user-{turn}"), Vec::new()),
                        assistant(format!("assistant-{turn}")),
                    ]
                })
                .collect(),
        );

        let newest = read_transcript_page(&path, None, 4).unwrap();
        assert_eq!(
            newest.messages(),
            vec![
                AgentMessage::user("user-4", Vec::new()),
                assistant("assistant-4"),
                AgentMessage::user("user-5", Vec::new()),
                assistant("assistant-5"),
            ]
        );
        assert!(newest.has_older);

        let older = read_transcript_page(&path, newest.next_cursor, 4).unwrap();
        assert_eq!(
            older.messages(),
            vec![
                AgentMessage::user("user-2", Vec::new()),
                assistant("assistant-2"),
                AgentMessage::user("user-3", Vec::new()),
                assistant("assistant-3"),
            ]
        );
        assert!(older.has_older);
    }

    #[test]
    fn transcript_page_expands_to_the_start_of_a_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        write_transcript(
            &path,
            vec![
                AgentMessage::user("older", Vec::new()),
                assistant("older answer"),
                AgentMessage::user("current", Vec::new()),
                AgentMessage::Custom {
                    custom_type: "thinking".into(),
                    payload: serde_json::json!({"text": "reasoning"}),
                },
                assistant("current answer"),
            ],
        );

        let page = read_transcript_page(&path, None, 2).unwrap();

        assert_eq!(page.messages().len(), 3);
        assert_eq!(
            page.messages()[0],
            AgentMessage::user("current", Vec::new())
        );
    }

    #[test]
    fn transcript_page_reads_legacy_nodes_and_ignores_torn_atomic_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let legacy = serde_json::json!({
            "id": "legacy-1",
            "parent_id": null,
            "timestamp": 1,
            "message": AgentMessage::user("legacy", Vec::new()),
        });
        std::fs::write(
            &path,
            format!("{}\n{ATOMIC_FRAME_SENTINEL}{{\"atomic_", legacy),
        )
        .unwrap();

        let page = read_transcript_page(&path, None, 1).unwrap();

        assert_eq!(
            page.messages(),
            vec![AgentMessage::user("legacy", Vec::new())]
        );
        assert!(!page.has_older);
    }

    #[test]
    fn transcript_page_cursor_is_clamped_after_file_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        write_transcript(
            &path,
            vec![AgentMessage::user("first", Vec::new()), assistant("second")],
        );

        let page = read_transcript_page(&path, Some(TranscriptCursor::new(u64::MAX)), 2).unwrap();

        assert_eq!(page.messages().len(), 2);
    }

    #[test]
    fn transcript_page_does_not_parse_an_unrequested_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut data = vec![b'x'; super::TRANSCRIPT_READ_CHUNK_BYTES * 2];
        data.push(b'\n');
        for (index, message) in [
            AgentMessage::user("recent", Vec::new()),
            assistant("recent answer"),
        ]
        .into_iter()
        .enumerate()
        {
            data.extend_from_slice(
                serde_json::to_string(&transcript_entry(index, message))
                    .unwrap()
                    .as_bytes(),
            );
            data.push(b'\n');
        }
        std::fs::write(&path, data).unwrap();

        let page = read_transcript_page(&path, None, 2).unwrap();

        assert_eq!(page.messages().len(), 2);
        assert!(page.has_older);
    }

    #[test]
    fn observational_records_use_data_sync_but_intents_use_full_sync() {
        let checkpoint = Record::StreamCheckpoint {
            id: "checkpoint".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            run_id: "run".into(),
            attempt: Some(1),
            request_id: TraceString::new("request").unwrap(),
            assistant_entry_id: None,
            text: None,
            reasoning: None,
            checkpoint_index: 1,
            byte_count: 10,
            fingerprint: TraceString::new("digest").unwrap(),
        };
        let operation = Record::OperationStarted {
            id: "run".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 1,
            source_leaf_id: None,
            intent: crate::harness::OperationIntent::Run,
        };
        let tool = Record::ToolStarted {
            id: "tool".into(),
            seq: 3,
            lane: "main".into(),
            timestamp: 1,
            run_id: "run".into(),
            assistant_entry_id: "assistant".into(),
            tool_index: 0,
            tool_call_id: "call".into(),
            tool_name: "read_file".into(),
            effective_args: serde_json::json!({}),
            result_entry_id: "result".into(),
            replay: crate::harness::ToolReplaySafety::Safe,
        };

        assert_eq!(checkpoint.sync_policy(), SyncPolicy::Data);
        assert_eq!(operation.sync_policy(), SyncPolicy::All);
        assert_eq!(tool.sync_policy(), SyncPolicy::All);
    }

    #[test]
    fn gated_append_does_not_reload_the_store_it_just_wrote() {
        super::LOAD_COUNT.with(|count| count.set(0));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single-store.jsonl");
        let store = JsonlStore::open(&path).unwrap();
        let mut harness = AgentHarness::with_events(store, HarnessEventHub::new(8));

        harness
            .append_entry_gated(user_entry("msg-1", "main"))
            .unwrap();
        harness.drive_to_completion().unwrap();

        assert_eq!(harness.store().entries().len(), 1);
        super::LOAD_COUNT.with(|count| assert_eq!(count.get(), 1));
    }

    #[test]
    fn reducing_a_jsonl_store_reuses_its_incremental_state() {
        super::super::reducer::BUILD_COUNT.with(|count| count.set(0));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached-reduction.jsonl");
        let store = JsonlStore::open(&path).unwrap();
        super::super::reducer::BUILD_COUNT.with(|count| assert_eq!(count.get(), 1));

        Reducer::reduce(&store).unwrap();

        super::super::reducer::BUILD_COUNT.with(|count| assert_eq!(count.get(), 1));
    }

    #[test]
    fn reload_rejects_an_entry_and_record_with_the_same_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicate-id.jsonl");
        let entry = user_entry("shared-id", "main");
        let record = Record::FactSet {
            id: "shared-id".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 0,
            run_id: None,
            key: "key".into(),
            value: "value".into(),
        };
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&entry).unwrap(),
                serde_json::to_string(&record).unwrap()
            ),
        )
        .unwrap();

        let error = JsonlStore::open(&path).unwrap_err();
        assert!(error.to_string().contains("shared-id"), "{error}");
    }

    #[test]
    fn malformed_modern_entry_does_not_fall_back_to_a_legacy_node() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("malformed-entry.jsonl");
        std::fs::write(
            &path,
            r#"{"id":"entry-1","parent_id":null,"lane":5,"seq":1,"timestamp":1,"message":{"role":"user","content":"hello"}}
"#,
        )
        .unwrap();

        assert!(JsonlStore::open(&path).is_err());
    }

    fn assert_completed_malformed_line_is_rejected(name: &str, line: &[u8]) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let mut bytes = line.to_vec();
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();
        assert!(JsonlStore::open(&path).is_err());
    }

    #[test]
    fn strict_open_rejects_completed_partial_atomic_sentinel_prefix() {
        assert_completed_malformed_line_is_rejected(
            "partial-sentinel.jsonl",
            &ATOMIC_FRAME_SENTINEL.as_bytes()[..10],
        );
    }

    #[test]
    fn strict_open_rejects_completed_full_atomic_sentinel_junk() {
        let line = format!("{ATOMIC_FRAME_SENTINEL}junk");
        assert_completed_malformed_line_is_rejected("sentinel-junk.jsonl", line.as_bytes());
    }

    #[test]
    fn strict_open_rejects_completed_arbitrary_torn_eof_suffix() {
        let line = format!("junk{TORN_EOF_SENTINEL}");
        assert_completed_malformed_line_is_rejected("torn-suffix.jsonl", line.as_bytes());
    }

    #[test]
    fn strict_open_rejects_completed_legacy_atomic_json_prefix() {
        let line = format!("{LEGACY_ATOMIC_FRAME_PREFIX}junk");
        assert_completed_malformed_line_is_rejected("legacy-prefix.jsonl", line.as_bytes());
    }

    fn assert_completed_malformed_transcript_line_is_rejected(name: &str, line: &[u8]) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let mut bytes = line.to_vec();
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();
        assert!(read_transcript_page(&path, None, 1).is_err());
    }

    #[test]
    fn transcript_page_rejects_completed_partial_atomic_sentinel_prefix() {
        assert_completed_malformed_transcript_line_is_rejected(
            "transcript-partial-sentinel.jsonl",
            &ATOMIC_FRAME_SENTINEL.as_bytes()[..10],
        );
    }

    #[test]
    fn transcript_page_rejects_completed_full_atomic_sentinel_junk() {
        let line = format!("{ATOMIC_FRAME_SENTINEL}junk");
        assert_completed_malformed_transcript_line_is_rejected(
            "transcript-sentinel-junk.jsonl",
            line.as_bytes(),
        );
    }

    #[test]
    fn transcript_page_rejects_completed_arbitrary_torn_eof_suffix() {
        let line = format!("junk{TORN_EOF_SENTINEL}");
        assert_completed_malformed_transcript_line_is_rejected(
            "transcript-torn-suffix.jsonl",
            line.as_bytes(),
        );
    }

    #[test]
    fn transcript_page_rejects_completed_legacy_atomic_json_prefix() {
        let line = format!("{LEGACY_ATOMIC_FRAME_PREFIX}junk");
        assert_completed_malformed_transcript_line_is_rejected(
            "transcript-legacy-prefix.jsonl",
            line.as_bytes(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_entry_append_does_not_advance_the_in_memory_leaf() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("failed-append.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store.append_entry(user_entry("first", "main")).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let result = store.append_entry(user_entry("not-durable", "main"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(result.is_err());
        assert_eq!(store.preferred_leaf("main").as_deref(), Some("first"));
        assert_eq!(store.entries().len(), 1);
    }

    /// Pins the incremental reducer: the live context advanced by
    /// guard/commit pairs must project identically to a fresh full
    /// reduction of the same history, in memory and after reload.
    #[test]
    fn incremental_appends_match_full_reduction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incremental.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();

        store.append_entry(user_entry("msg-1", "main")).unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                source_leaf_id: Some("msg-1".into()),
                intent: crate::harness::OperationIntent::Run,
            })
            .unwrap();
        store
            .append_entry(crate::harness::Entry::new(
                "asst-1",
                Some("msg-1".to_owned()),
                "main",
                0,
                0,
                AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_protocol::RuntimeToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: threadlane_protocol::RuntimeToolCallFunction {
                            name: "run".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                false,
            ))
            .unwrap();
        store
            .append_entry(crate::harness::Entry::new(
                "res-1",
                Some("asst-1".to_owned()),
                "main",
                0,
                0,
                AgentMessage::Tool {
                    tool_call_id: "call-1".into(),
                    name: "run".into(),
                    content: "ok".into(),
                    is_error: false,
                    terminate: false,
                },
                false,
            ))
            .unwrap();
        store
            .append_record(Record::ToolStarted {
                id: "tool-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: "run-1".into(),
                assistant_entry_id: "asst-1".into(),
                tool_index: 0,
                tool_call_id: "call-1".into(),
                tool_name: "run".into(),
                effective_args: serde_json::json!({}),
                result_entry_id: "res-1".into(),
                replay: crate::harness::ToolReplaySafety::Safe,
            })
            .unwrap();
        store
            .append_record(Record::ToolFinished {
                id: "tool-finish-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: "run-1".into(),
                tool_call_id: "call-1".into(),
                result_entry_id: "res-1".into(),
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::StepAttempt {
                id: "step-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: "run-1".into(),
                attempt: 1,
                result_entry_id: "res-1".into(),
                compaction_reason: None,
            })
            .unwrap();
        store
            .append_record(Record::RetryScheduled {
                id: "retry-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 10,
                run_id: "run-1".into(),
                attempt: 2,
                retry_at: 20,
                reason: "rate limit".into(),
            })
            .unwrap();
        store
            .append_record(Record::RetryConsumed {
                id: "retry-consumed-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 20,
                run_id: "run-1".into(),
                attempt: 2,
            })
            .unwrap();
        store
            .append_record(Record::QueueEnqueued {
                id: "queue-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: Some("run-1".into()),
                queue: crate::harness::QueueKind::FollowUp,
                priority: None,
                target: crate::harness::ProvisionedEntry::new(
                    "queued-msg",
                    None,
                    AgentMessage::user("q", vec![]),
                ),
            })
            .unwrap();
        store
            .append_record(Record::QueueConsumed {
                id: "queue-consumed-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: "run-1".into(),
                entry_id: "queued-msg".into(),
            })
            .unwrap();
        store
            .append_record(Record::FactSet {
                id: "fact-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: None,
                key: "model".into(),
                value: "test-model".into(),
            })
            .unwrap();
        store.append_entry(user_entry("sub-msg", "lane-2")).unwrap();
        store
            .append_record(Record::OperationFinished {
                id: "finish-1".into(),
                seq: store.next_sequence(),
                lane: "main".into(),
                timestamp: 1,
                run_id: "run-1".into(),
                outcome: crate::harness::OperationOutcome::Completed,
                error: None,
            })
            .unwrap();

        let assert_equivalent = |label: &str| {
            let live = store.reduced_state();
            let fresh = Reducer::reduce(&store).expect("full reduction succeeds");
            assert_eq!(
                live.lanes.len(),
                fresh.lanes.len(),
                "{label}: lane count diverged"
            );
            for (live_lane, fresh_lane) in live.lanes.iter().zip(fresh.lanes.iter()) {
                assert_eq!(live_lane, fresh_lane, "{label}: lane state diverged");
            }
        };

        // Live incremental projection matches a fresh reduction at every step.
        assert_equivalent("after interleaved appends");

        // The same holds after a reload from disk.
        drop(store);
        let reloaded = JsonlStore::open(&path).unwrap();
        let fresh_after_reload = Reducer::reduce(&reloaded).unwrap();
        let reloaded_state = reloaded.reduced_state();
        for (reloaded_lane, fresh_lane) in reloaded_state
            .lanes
            .iter()
            .zip(fresh_after_reload.lanes.iter())
        {
            assert_eq!(reloaded_lane, fresh_lane, "reload: lane state diverged");
        }
    }

    #[test]
    fn legacy_metadata_records_get_distinct_virtual_sequences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"session_metadata\",\"name\":\"One\",\"model\":\"model\",\"title_attempted\":true}\n",
        )
        .unwrap();

        let (_, _, _, records) = read_entries(&path).unwrap();
        let sequences = records
            .iter()
            .map(|record| record.seq())
            .collect::<Vec<_>>();
        assert_eq!(sequences.len(), 3);
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn context_manifest_captured_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();

        let items = vec![
            ContextManifestItem {
                position: 0,
                source: ContextItemSource::SystemPrompt,
                entry_id: None,
                role: TraceString::new("system").unwrap(),
                token_estimate: 42,
                status: ContextItemStatus::Active,
                digest_sha256: TraceString::new("abc123sha").unwrap(),
                label: None,
            },
            ContextManifestItem {
                position: 1,
                source: ContextItemSource::Message,
                entry_id: Some(TraceString::new("entry-1").unwrap()),
                role: TraceString::new("user").unwrap(),
                token_estimate: 15,
                status: ContextItemStatus::Active,
                digest_sha256: TraceString::new("def456sha").unwrap(),
                label: None,
            },
            ContextManifestItem {
                position: 2,
                source: ContextItemSource::ToolSchema,
                entry_id: None,
                role: TraceString::new("tools").unwrap(),
                token_estimate: 120,
                status: ContextItemStatus::Active,
                digest_sha256: TraceString::new("toolssha789").unwrap(),
                label: Some(TraceString::new("3 tools").unwrap()),
            },
        ];

        let manifest_record = Record::ContextManifestCaptured {
            id: "context-manifest-run1-req1".into(),
            seq: store.next_sequence(),
            lane: "main".into(),
            timestamp: 1234567890,
            run_id: "run1".into(),
            attempt: 1,
            request_id: TraceString::new("provider-req-1").unwrap(),
            total_estimated_tokens: Some(177),
            effective_model: None,
            context_limit: None,
            context_limit_is_estimate: false,
            compaction_generation: 0,
            items,
        };

        store.append_record(manifest_record).unwrap();
        drop(store);

        // Verify reading back from file
        let reloaded = JsonlStore::open(&path).unwrap();
        assert_eq!(reloaded.records().len(), 1);
        match &reloaded.records()[0] {
            Record::ContextManifestCaptured {
                id,
                seq,
                lane,
                run_id,
                attempt,
                request_id,
                total_estimated_tokens,
                items,
                ..
            } => {
                assert_eq!(id, "context-manifest-run1-req1");
                assert_eq!(*seq, 1);
                assert_eq!(lane, "main");
                assert_eq!(run_id, "run1");
                assert_eq!(*attempt, 1);
                assert_eq!(request_id.as_str(), "provider-req-1");
                assert_eq!(*total_estimated_tokens, Some(177));
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].source, ContextItemSource::SystemPrompt);
                assert_eq!(items[1].source, ContextItemSource::Message);
                assert_eq!(items[2].source, ContextItemSource::ToolSchema);
                assert_eq!(items[2].label.as_ref().unwrap().as_str(), "3 tools");
            }
            other => panic!("expected ContextManifestCaptured, got: {other:?}"),
        }

        // Verify reducer handles the record gracefully
        let reduced = Reducer::reduce(&reloaded).unwrap();
        assert_eq!(reduced.lanes.len(), 1);
        assert_eq!(reduced.lanes[0].name, "main");
    }

    #[test]
    fn legacy_context_manifest_deserializes_without_new_metadata() {
        let json = r#"{"ContextManifestCaptured":{"id":"manifest","seq":2,"lane":"main","timestamp":2,"run_id":"run","attempt":1,"request_id":"request","items":[]}}"#;
        let record: Record = serde_json::from_str(json).unwrap();
        let Record::ContextManifestCaptured {
            effective_model,
            context_limit,
            context_limit_is_estimate,
            compaction_generation,
            ..
        } = record
        else {
            panic!("expected manifest")
        };
        assert_eq!(effective_model, None);
        assert_eq!(context_limit, None);
        assert!(!context_limit_is_estimate);
        assert_eq!(compaction_generation, 0);
    }

    #[test]
    fn compaction_telemetry_round_trips() {
        let record = Record::ContextCompacted {
            id: "compaction-42".into(),
            seq: 42,
            lane: "main".into(),
            timestamp: 42,
            run_id: "run".into(),
            generation: 2,
            reason: CompactionReason::AdaptiveBudget,
            effective_model: TraceString::new("model").unwrap(),
            context_limit: 128_000,
            context_limit_is_estimate: true,
            pre_tokens: 742_000,
            post_tokens: 118_000,
            retained_tail_target: 32_000,
            retained_tail_tokens: 30_000,
            compacted_messages: 40,
        };
        let encoded = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<Record>(&encoded).unwrap(), record);
    }

    #[test]
    fn transcript_page_orders_compaction_marker_without_exposing_context_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut restored = transcript_entry(2, assistant("before"));
        restored.surface_op = crate::harness::SurfaceOperation::Replace {
            start_seq: 2,
            end_seq: 1,
            source_event_seqs: vec![0],
        };
        let lines = vec![
            serde_json::to_string(&transcript_entry(
                0,
                AgentMessage::User {
                    content: "before".into(),
                },
            ))
            .unwrap(),
            serde_json::to_string(&transcript_entry(
                1,
                AgentMessage::Custom {
                    custom_type: "compaction_summary".into(),
                    payload: serde_json::json!({"summary": "internal"}),
                },
            ))
            .unwrap(),
            serde_json::to_string(&restored).unwrap(),
            serde_json::to_string(&Record::ContextCompacted {
                id: "compaction-marker".into(),
                seq: 3,
                lane: "main".into(),
                timestamp: 3,
                run_id: "run".into(),
                generation: 1,
                reason: CompactionReason::AdaptiveBudget,
                effective_model: TraceString::new("model").unwrap(),
                context_limit: 128_000,
                context_limit_is_estimate: false,
                pre_tokens: 130_000,
                post_tokens: 30_000,
                retained_tail_target: 32_000,
                retained_tail_tokens: 25_000,
                compacted_messages: 10,
            })
            .unwrap(),
            serde_json::to_string(&transcript_entry(3, assistant("after"))).unwrap(),
        ];
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        let page = read_transcript_page(&path, None, 2).unwrap();
        assert!(matches!(page.items[0], TranscriptItem::Message(_)));
        assert!(matches!(page.items[1], TranscriptItem::ContextCompacted(_)));
        assert!(matches!(page.items[2], TranscriptItem::Message(_)));
        assert!(!page.items.iter().any(|item| matches!(item,
            TranscriptItem::Message(AgentMessage::Custom { custom_type, .. }) if custom_type == "compaction_summary"
        )));
    }

    #[test]
    fn every_atomic_frame_cut_is_quarantined_without_rewriting_prior_bytes() {
        let frame = AtomicBatchLine {
            atomic_batch: vec![AtomicBatchItem::Entry(transcript_entry(
                2,
                assistant("framed"),
            ))],
        };
        let mut encoded = ATOMIC_FRAME_SENTINEL.as_bytes().to_vec();
        encoded.extend(serde_json::to_vec(&frame).unwrap());

        for cut in 0..encoded.len() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("atomic-cut-{cut}.jsonl"));
            let mut store = JsonlStore::open(&path).unwrap();
            store.append_entry(user_entry("root", "main")).unwrap();
            drop(store);
            let canonical = std::fs::read(&path).unwrap();
            let mut bytes = canonical.clone();
            bytes.extend_from_slice(&encoded[..cut]);
            std::fs::write(&path, &bytes).unwrap();

            let mut recovered = JsonlStore::open(&path).unwrap();
            recovered
                .append_record(Record::FactSet {
                    id: "fact-after-crash".into(),
                    seq: 0,
                    lane: "main".into(),
                    timestamp: 1,
                    run_id: None,
                    key: "recovered".into(),
                    value: "true".into(),
                })
                .unwrap();
            drop(recovered);

            let durable = std::fs::read(&path).unwrap();
            assert_eq!(
                &durable[..canonical.len()],
                canonical.as_slice(),
                "cut {cut}"
            );
            assert_eq!(
                &durable[canonical.len()..canonical.len() + cut],
                &encoded[..cut],
                "cut {cut}"
            );
            assert!(
                JsonlStore::open(&path)
                    .unwrap()
                    .records()
                    .iter()
                    .any(|record| record.id() == "fact-after-crash"),
                "cut {cut}"
            );
            let page = read_transcript_page(&path, None, 1).unwrap();
            assert_eq!(page.messages().len(), 1, "cut {cut}");
        }

        // Compatibility with frames written before the sentinel contract: an
        // arbitrary invalid EOF fragment is sealed with a reserved suffix.
        for fragment in [b"{".as_slice(), b"{\"at".as_slice()] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("legacy-cut.jsonl");
            let mut store = JsonlStore::open(&path).unwrap();
            store.append_entry(user_entry("root", "main")).unwrap();
            drop(store);
            let canonical = std::fs::read(&path).unwrap();
            let mut bytes = canonical.clone();
            bytes.extend_from_slice(fragment);
            std::fs::write(&path, bytes).unwrap();
            let mut recovered = JsonlStore::open(&path).unwrap();
            recovered
                .append_record(Record::FactSet {
                    id: "later".into(),
                    seq: 0,
                    lane: "main".into(),
                    timestamp: 1,
                    run_id: None,
                    key: "k".into(),
                    value: "v".into(),
                })
                .unwrap();
            drop(recovered);
            let durable = std::fs::read(&path).unwrap();
            assert_eq!(&durable[..canonical.len()], canonical.as_slice());
            assert_eq!(
                &durable[canonical.len()..canonical.len() + fragment.len()],
                fragment
            );
            JsonlStore::open(&path).unwrap();
            let page = read_transcript_page(&path, None, 1).unwrap();
            assert_eq!(page.messages().len(), 1);
        }
    }

    #[test]
    fn transcript_paging_losslessly_expands_multi_visible_atomic_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atomic-transcript.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string(&transcript_entry(
                    1,
                    AgentMessage::user("older", Vec::new())
                ))
                .unwrap()
            ),
        )
        .unwrap();
        let frame = AtomicBatchLine {
            atomic_batch: vec![
                AtomicBatchItem::Entry(transcript_entry(
                    2,
                    AgentMessage::user("new turn", Vec::new()),
                )),
                AtomicBatchItem::Record(Record::ContextCompacted {
                    id: "marker".into(),
                    seq: 3,
                    lane: "main".into(),
                    timestamp: 3,
                    run_id: "run".into(),
                    generation: 1,
                    reason: CompactionReason::AdaptiveBudget,
                    effective_model: TraceString::new("model").unwrap(),
                    context_limit: 100,
                    context_limit_is_estimate: false,
                    pre_tokens: 90,
                    post_tokens: 40,
                    retained_tail_target: 20,
                    retained_tail_tokens: 20,
                    compacted_messages: 2,
                }),
                AtomicBatchItem::Entry(transcript_entry(4, assistant("answer"))),
            ],
        };
        append_atomic_batch_line(&path, &frame).unwrap();

        let newest = read_transcript_page(&path, None, 1).unwrap();
        assert_eq!(newest.items.len(), 3);
        assert!(matches!(&newest.items[0], TranscriptItem::Message(message) if message.is_user()));
        assert!(matches!(
            &newest.items[1],
            TranscriptItem::ContextCompacted(_)
        ));
        assert!(matches!(&newest.items[2], TranscriptItem::Message(message) if !message.is_user()));
        assert_eq!(newest.messages().len(), 2);
        assert!(newest.has_older);

        let older = read_transcript_page(&path, newest.next_cursor, 1).unwrap();
        assert_eq!(older.messages().len(), 1);
        assert!(!older.has_older);
    }
}
