use super::reducer::ReductionContext;
use super::store::SessionStore;
use super::types::{Entry, Record, ReduceError};
use threadlane_protocol::{AgentMessage, PlanItem};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashSet as IdSet;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::SystemTime;

fn stamp_lifecycle_time(record: &mut Record) {
    if let Record::OperationStarted { wall_time_ms, .. }
        | Record::OperationFinished { wall_time_ms, .. }
        | Record::AbortObserved { wall_time_ms, .. } = record
    {
        if wall_time_ms.is_none() {
            *wall_time_ms = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .and_then(|duration| u64::try_from(duration.as_millis()).ok());
        }
    }
}

#[cfg(test)]
thread_local! {
    static LOAD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FULL_RELOAD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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

/// Process-wide serialiser for all session-file appends (single lines and
/// atomic batches) so concurrent `append_entry` / `append_atomic_batch`
/// traffic to the same path cannot interleave.
fn session_append_lock() -> &'static Mutex<()> {
    static APPEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    APPEND_LOCK.get_or_init(|| Mutex::new(()))
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

/// Registry key for the writer lease. The session file itself usually does
/// not exist yet on first open, so canonicalizing the file path directly
/// fails and different spellings (`a.jsonl` vs `./a.jsonl`, symlinked
/// parents) would split into independent gates and lock files. Canonicalize
/// the parent directory (which must exist for the file to be openable) and
/// join the file name instead.
fn canonical_writer_key(path: &Path) -> PathBuf {
    if let (Some(parent), Some(name)) = (
        path.parent().filter(|p| !p.as_os_str().is_empty()),
        path.file_name(),
    ) {
        if let Ok(canonical_parent) = parent.canonicalize() {
            return canonical_parent.join(name);
        }
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn writer_claim(path: &Path) -> io::Result<Arc<WriterClaim>> {
    static CLAIMS: OnceLock<Mutex<HashMap<PathBuf, Weak<WriterClaim>>>> = OnceLock::new();
    let canonical = canonical_writer_key(path);
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
    message: threadlane_protocol::AgentMessage,
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
    Message(threadlane_protocol::AgentMessage),
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
    fn messages(&self) -> Vec<threadlane_protocol::AgentMessage> {
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
    let Some((tail_start, tail)) = read_unterminated_tail(file, end)? else {
        return Ok(end);
    };
    if parse_transcript_line(path, tail_start, &tail, true)?.is_some() {
        Ok(end)
    } else {
        Ok(tail_start)
    }
}

/// Normal appends and transcript reads inspect one byte. Recovery scans the
/// entire last line, since valid records can exceed a single probe window.
fn read_unterminated_tail(
    file: &mut (impl Read + Seek),
    end: u64,
) -> io::Result<Option<(u64, Vec<u8>)>> {
    if end == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(end - 1))?;
    let mut final_byte = [0];
    file.read_exact(&mut final_byte)?;
    if final_byte[0] == b'\n' {
        return Ok(None);
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
    Ok(Some((tail_start, tail)))
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
        Err(_) if is_recoverable_journal_fragment(bytes, is_physical_eof) => Ok(None),
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
                        threadlane_protocol::AgentMessage::Custom {
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
                threadlane_protocol::AgentMessage::Custom {
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
    session_mtime: Option<SystemTime>,
    harness_mtime: Option<SystemTime>,
    /// Highest sequence across entries and records, maintained incrementally
    /// so sequence allocation does not rescan the whole file.
    max_seq: u64,
    /// Entry ids only (valid parents for new entries), maintained
    /// incrementally for O(1) parent and duplicate checks.
    entry_ids: HashSet<String>,
    /// `entry_id -> index into entries`, maintained incrementally for O(1)
    /// id lookups (entries are append-only, so positions never shift).
    entry_positions: HashMap<String, usize>,
    /// Record ids, maintained incrementally for O(1) duplicate checks.
    record_ids: HashSet<String>,
    /// Streaming reduction state advanced by guard/commit pairs on append;
    /// rebuilt wholesale whenever the file is reloaded.
    reduction: ReductionContext,
    /// Count of session-journal lines that parsed into values, maintained
    /// incrementally so tail classification derives the same positional
    /// indexes (legacy seq fallbacks, fact ids) as a full reload.
    journal_line_count: usize,
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
        let (session_id, preferred_leaf, entries, records, journal_line_count) =
            Self::load_parts(&path)?;
        let (session_file_len, session_mtime) = file_fingerprint(&path)?;
        let (harness_file_len, harness_mtime) =
            file_fingerprint(&path.with_extension("harness.jsonl"))?;
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
        let entry_positions = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
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
            session_mtime,
            harness_mtime,
            max_seq,
            entry_ids,
            entry_positions,
            record_ids,
            reduction,
            journal_line_count,
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
        self.entry_positions = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect();
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
        let (session_len, session_mtime) = file_fingerprint(&self.path)?;
        let (harness_len, harness_mtime) =
            file_fingerprint(&self.path.with_extension("harness.jsonl"))?;
        self.session_file_len = session_len;
        self.session_mtime = session_mtime;
        self.harness_file_len = harness_len;
        self.harness_mtime = harness_mtime;
        Ok(())
    }

    fn is_fresh(&self) -> io::Result<bool> {
        let (session_len, session_mtime) = file_fingerprint(&self.path)?;
        let (harness_len, harness_mtime) =
            file_fingerprint(&self.path.with_extension("harness.jsonl"))?;
        Ok(self.session_file_len == session_len
            && self.session_mtime == session_mtime
            && self.harness_file_len == harness_len
            && self.harness_mtime == harness_mtime)
    }

    fn load_parts(
        path: &Path,
    ) -> io::Result<(String, Option<String>, Vec<Entry>, Vec<Record>, usize)> {
        let lines = read_strict::<SessionLine>(path)?;
        let journal_line_count = lines.len();
        let (session_id, preferred_leaf, entries, mut records) =
            classify_lines(path, lines, 0);
        let record_path = path.with_extension("harness.jsonl");
        records.extend(read_strict(&record_path)?);
        records.sort_by_key(Record::seq);
        validate_harness_records(&records, path)?;
        Ok((
            session_id,
            preferred_leaf,
            entries,
            records,
            journal_line_count,
        ))
    }

    /// Read recoverable main-lane entries without reducing a damaged journal.
    /// Repeated durable identities are one entry, but identical messages with
    /// distinct identities remain separate conversation occurrences.
    pub fn recover_main_entries(path: &Path) -> io::Result<Vec<Entry>> {
        let lines = read_strict::<SessionLine>(path)?;
        let (_, _, entries, _) = classify_lines(path, lines, 0);
        let mut seen = std::collections::HashSet::new();
        Ok(entries
            .into_iter()
            .filter(|entry| entry.lane == "main" && seen.insert(entry.id.clone()))
            .collect())
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

    /// Whether a `ToolStarted` intent for `(run_id, tool_call_id)` is already
    /// committed, without rescanning the record stream.
    pub fn has_tool_started(&self, run_id: &str, tool_call_id: &str) -> bool {
        self.reduction.has_tool_started(run_id, tool_call_id)
    }

    /// The canonical committed tool slot, including its owning lane. Uses the
    /// same incremental index as `has_tool_started`, including after reload.
    pub fn tool_state_for_call(
        &self,
        run_id: &str,
        tool_call_id: &str,
    ) -> Option<(&str, &super::types::ToolState)> {
        self.reduction.tool_state_for_call(run_id, tool_call_id)
    }

    /// Latest entry on `lane` whose message equals `message`.
    pub fn find_entry_by_message(
        &self,
        lane: &str,
        message: &AgentMessage,
    ) -> Option<&Entry> {
        self.reduction
            .entry_id_by_message(lane, message)
            .and_then(|id| self.entry(id))
    }

    /// Latest assistant entry declaring `tool_call_id`; `lane` scopes the
    /// lookup to that lane while `None` searches all lanes.
    pub fn assistant_entry_for_call(
        &self,
        lane: Option<&str>,
        tool_call_id: &str,
    ) -> Option<&Entry> {
        self.reduction
            .assistant_entry_for_call(lane, tool_call_id)
            .and_then(|id| self.entry(id))
    }

    /// Lane and start sequence of a committed, currently open operation.
    /// Uses the incremental projection without cloning or scanning history.
    pub fn open_operation_lane(&self, run_id: &str) -> Option<(&str, u64)> {
        self.reduction.open_operation_lane(run_id)
    }

    /// Latest declaration for this call in the owning operation. A matching
    /// call ID on another lane or before this run started is not its identity.
    pub fn assistant_entry_for_run_call(
        &self,
        run_id: &str,
        tool_call_id: &str,
    ) -> Option<&Entry> {
        let (lane, start_seq) = self.open_operation_lane(run_id)?;
        self.assistant_entry_for_call(Some(lane), tool_call_id)
            .filter(|entry| entry.seq > start_seq)
    }

    /// Latest assistant entry on `lane`.
    pub fn last_assistant_entry(&self, lane: &str) -> Option<&Entry> {
        self.reduction
            .last_assistant_entry_id(lane)
            .and_then(|id| self.entry(id))
    }
}

impl SessionStore for JsonlStore {
    fn session_id(&self) -> &str {
        &self.session_id
    }
    fn reduced_state(&self) -> Option<super::types::ReducedState> {
        Some(self.reduced_state())
    }
    fn open_operation_lane(&self, run_id: &str) -> Option<(&str, u64)> {
        JsonlStore::open_operation_lane(self, run_id)
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

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entry_positions.get(id).map(|index| &self.entries[*index])
    }

    fn branch(&self, leaf_id: Option<&str>, limit: usize) -> Vec<Entry> {
        if limit == 0 {
            return Vec::new();
        }
        let mut current = match leaf_id {
            Some(id) => self.entry(id),
            None => self.entries.last(),
        };
        let mut branch = Vec::new();
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
                    stamp_lifecycle_time(&mut record);
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
        let batch = AtomicBatchLine {
            atomic_batch: items,
        };
        let written = append_atomic_batch_line(&self.path, &batch)?;
        // The writer gate and append lock already serialized this commit, so a
        // file that grew by exactly the bytes we wrote needs no re-parse: the
        // pre-validated reduction clone becomes the committed state and the
        // items join the in-memory streams. Any other delta means a foreign
        // writer interleaved (or the side file changed) — fold or reload.
        let (new_len, new_mtime) = file_fingerprint(&self.path)
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        let (harness_len, harness_mtime) =
            file_fingerprint(&self.path.with_extension("harness.jsonl"))
                .map_err(|error| ReduceError::Storage(error.to_string()))?;
        if new_len.checked_sub(self.session_file_len) == Some(written)
            && harness_len == self.harness_file_len
            && harness_mtime == self.harness_mtime
        {
            self.session_file_len = new_len;
            self.session_mtime = new_mtime;
            self.journal_line_count += 1;
            self.reduction = reduction;
            if let Some(leaf) = final_main_leaf {
                self.preferred_leaf = Some(leaf);
            }
            for item in batch.atomic_batch {
                match item {
                    AtomicBatchItem::Entry(entry) => {
                        self.max_seq = self.max_seq.max(entry.seq);
                        self.entry_ids.insert(entry.id.clone());
                        self.entry_positions.insert(entry.id.clone(), self.entries.len());
                        self.entries.push(entry);
                    }
                    AtomicBatchItem::Record(record) => {
                        self.max_seq = self.max_seq.max(record.seq());
                        self.record_ids.insert(record.id().to_owned());
                        self.records.push(record);
                    }
                }
            }
            return Ok(());
        }
        self.reload_unlocked()
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
        (self.session_file_len, self.session_mtime) = file_fingerprint(&self.path)
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        if entry.lane == "main" {
            let leaf = entry.id.clone();
            self.preferred_leaf = Some(leaf.clone());
            self.reduction.set_preferred_leaf_main(leaf);
        }
        self.reduction.commit_entry(&entry);
        self.max_seq = entry.seq;
        self.entry_ids.insert(entry.id.clone());
        self.entry_positions.insert(entry.id.clone(), self.entries.len());
        self.entries.push(entry);
        self.journal_line_count += 1;
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
        stamp_lifecycle_time(&mut record);
        self.reduction.record_guard(&record)?;
        append_json_line(&self.path, &record, record.sync_policy())?;
        (self.session_file_len, self.session_mtime) = file_fingerprint(&self.path)
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.reduction.commit_record(&record);
        self.max_seq = record.seq();
        self.record_ids.insert(record.id().to_owned());
        self.records.push(record);
        self.journal_line_count += 1;
        Ok(())
    }
}

impl JsonlStore {
    pub(crate) fn append_plan(&mut self, plan: &threadlane_protocol::SessionPlan) -> Result<(), ReduceError> {
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

    pub fn next_sequence(&self) -> u64 {
        self.next_seq()
    }

    /// Reloads in-memory state from disk when another writer has appended.
    /// Caller must hold the claim gate.
    fn reload_unlocked(&mut self) -> Result<(), ReduceError> {
        if self.fold_appended_tail()? {
            return Ok(());
        }
        #[cfg(test)]
        FULL_RELOAD_COUNT.with(|count| count.set(count.get() + 1));
        let refreshed = Self::load_parts(&self.path)
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.session_id = refreshed.0;
        self.preferred_leaf = refreshed.1;
        self.entries = refreshed.2;
        self.records = refreshed.3;
        self.journal_line_count = refreshed.4;
        self.refresh_file_lengths()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        self.rebuild_derived_state()
            .map_err(|error| ReduceError::Storage(error.to_string()))
    }

    /// Applies foreign appends by parsing only the bytes past the last
    /// fingerprint instead of re-reading the whole journal. Returns `false`
    /// whenever the file did anything other than grow cleanly — truncation,
    /// a same-length rewrite, a side-file change, a mid-line offset, an
    /// unparsable or mis-sequenced tail — so the caller can fall back to the
    /// canonical full reload (which also reports canonical errors).
    fn fold_appended_tail(&mut self) -> Result<bool, ReduceError> {
        let (session_len, session_mtime) = file_fingerprint(&self.path)
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        let (harness_len, harness_mtime) =
            file_fingerprint(&self.path.with_extension("harness.jsonl"))
                .map_err(|error| ReduceError::Storage(error.to_string()))?;
        if harness_len != self.harness_file_len
            || harness_mtime != self.harness_mtime
            || session_len < self.session_file_len
        {
            return Ok(false);
        }
        if session_len == self.session_file_len {
            // Same length with a different mtime is a rewrite — full reload.
            return Ok(session_mtime == self.session_mtime);
        }
        let Some(lines) = read_tail::<SessionLine>(&self.path, self.session_file_len)
            .map_err(|error| ReduceError::Storage(error.to_string()))?
        else {
            return Ok(false);
        };
        let parsed = lines.len();
        let (_session_id, tail_preferred, entries, mut records) =
            classify_lines(&self.path, lines, self.journal_line_count);
        // Legacy seq-0 records get virtual sequences above every durable one,
        // matching the full-load assignment.
        let real_max = entries
            .iter()
            .map(|entry| entry.seq)
            .chain(records.iter().map(Record::seq))
            .max()
            .unwrap_or(0)
            .max(self.max_seq);
        let mut next_seq = real_max + 1;
        for record in &mut records {
            if record.seq() == 0 {
                *record = record.clone().with_seq(next_seq);
                next_seq += 1;
            }
        }
        // A foreign item numbered inside committed history means a stale or
        // interleaved writer; only the full sorted merge reproduces that file.
        if entries
            .iter()
            .map(|entry| entry.seq)
            .chain(records.iter().map(Record::seq))
            .any(|seq| seq <= self.max_seq)
        {
            return Ok(false);
        }
        // Match the full load's global record sort; entries keep file order.
        records.sort_by_key(Record::seq);
        let effective_preferred = tail_preferred.clone().or_else(|| self.preferred_leaf.clone());
        let mut reduction = self.reduction.clone();
        if reduction
            .fold(&entries, &records, &|lane: &str| {
                (lane == "main").then(|| effective_preferred.clone()).flatten()
            })
            .is_err()
        {
            return Ok(false);
        }
        self.reduction = reduction;
        if let Some(leaf) = tail_preferred {
            self.preferred_leaf = Some(leaf);
        }
        for entry in entries {
            self.max_seq = self.max_seq.max(entry.seq);
            self.entry_ids.insert(entry.id.clone());
            self.entry_positions.insert(entry.id.clone(), self.entries.len());
            self.entries.push(entry);
        }
        for record in records {
            self.max_seq = self.max_seq.max(record.seq());
            self.record_ids.insert(record.id().to_owned());
            self.records.push(record);
        }
        self.session_file_len = session_len;
        self.session_mtime = session_mtime;
        self.journal_line_count += parsed;
        Ok(true)
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
            | Self::ProviderResponseAttached { .. } => SyncPolicy::Data,
            Self::RunContextCaptured { .. }
            | Self::ProviderRequestStarted { .. }
            | Self::ProviderRequestFinished { .. }
            | Self::StreamCheckpoint { .. } => SyncPolicy::All,
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
    // Serialization failures must not leave a partial durable record.
    let mut encoded = serde_json::to_vec(value).map_err(io::Error::other)?;
    encoded.push(b'\n');
    // Process-wide append lock shared with atomic batches; the session
    // writer lease handles cross-process writers.
    let _guard = session_append_lock()
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
    file.write_all(&encoded)?;
    match sync_policy {
        SyncPolicy::All => file.sync_all(),
        SyncPolicy::Data => file.sync_data(),
    }
}

fn prepare_append_boundary(file: &mut fs::File) -> io::Result<()> {
    let len = file.metadata()?.len();
    let Some((_, tail_line)) = read_unterminated_tail(file, len)? else {
        return Ok(());
    };
    let payload = atomic_frame_payload(&tail_line).unwrap_or(&tail_line);
    if serde_json::from_slice::<serde_json::Value>(payload).is_err() {
        // Quarantine any torn tail (atomic fragment or torn single-line
        // JSON) so the next strict read can skip it instead of bricking
        // the whole session.
        file.seek(SeekFrom::End(0))?;
        file.write_all(TORN_EOF_SENTINEL.as_bytes())?;
    }
    file.seek(SeekFrom::End(0))?;
    file.write_all(b"\n")
}

/// Length plus mtime so a same-length truncate-and-rewrite is still detected
/// as stale. Missing files fingerprint as zero length with no mtime.
fn file_fingerprint(path: &Path) -> io::Result<(u64, Option<SystemTime>)> {
    match fs::metadata(path) {
        Ok(metadata) => Ok((metadata.len(), metadata.modified().ok())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((0, None)),
        Err(error) => Err(error),
    }
}

fn append_json_line<T: serde::Serialize>(
    path: &Path,
    value: &T,
    sync_policy: SyncPolicy,
) -> Result<(), ReduceError> {
    append_session_json_line_with_policy(path, value, sync_policy)
        .inspect_err(|error| {
            tracing::error!(session_file = %path.display(), %error, "session journal append failed");
        })
        .map_err(|error| ReduceError::Storage(error.to_string()))
}

/// Appends one atomic frame and returns the number of bytes the file grew,
/// so callers can confirm no foreign writer interleaved between open and
/// sync without re-parsing the journal.
fn append_atomic_batch_line(path: &Path, value: &AtomicBatchLine) -> Result<u64, ReduceError> {
    let encoded =
        serde_json::to_vec(value).map_err(|error| ReduceError::Storage(error.to_string()))?;
    let _guard = session_append_lock()
        .lock()
        .map_err(|error| ReduceError::Storage(error.to_string()))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .inspect_err(|error| {
            tracing::error!(session_file = %path.display(), %error, "session journal open failed");
        })
        .map_err(|error| ReduceError::Storage(error.to_string()))?;
    let before = file
        .metadata()
        .map_err(|error| ReduceError::Storage(error.to_string()))?
        .len();
    prepare_append_boundary(&mut file)
        .and_then(|_| file.write_all(ATOMIC_FRAME_SENTINEL.as_bytes()))
        .and_then(|_| file.write_all(&encoded))
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .and_then(|_| file.metadata().map(|metadata| metadata.len() - before))
        .inspect_err(|error| {
            tracing::error!(session_file = %path.display(), %error, "session journal atomic append failed");
        })
        .map_err(|error| ReduceError::Storage(error.to_string()))
}

/// Typed classification entry point retained for focused compatibility tests.
#[cfg(test)]
fn read_entries(path: &Path) -> io::Result<(String, Option<String>, Vec<Entry>, Vec<Record>)> {
    let lines = read_strict::<SessionLine>(path)?;
    Ok(classify_lines(path, lines, 0))
}

/// `line_offset` is the number of already-classified journal lines before
/// this batch; positional indexes stay identical to a full-file parse.
fn classify_lines(
    path: &Path,
    lines: Vec<SessionLine>,
    line_offset: usize,
) -> (String, Option<String>, Vec<Entry>, Vec<Record>) {
    let session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "session".into());
    let mut preferred_leaf = None;
    let mut entries = Vec::new();
    let mut records = Vec::new();
    for (ordinal, line) in lines.into_iter().enumerate() {
        let index = line_offset + ordinal;
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
                    let plan = threadlane_protocol::SessionPlan { explanation, items };
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

fn is_recoverable_journal_fragment(bytes: &[u8], is_physical_eof: bool) -> bool {
    if is_physical_eof {
        let payload = atomic_frame_payload(bytes).unwrap_or(bytes);
        if is_atomic_frame_fragment(bytes)
            || serde_json::from_slice::<serde_json::Value>(payload)
                .is_err_and(|error| error.is_eof())
        {
            return true;
        }
    }
    let Some(stripped) = bytes.strip_suffix(TORN_EOF_SENTINEL.as_bytes()) else {
        return false;
    };
    if is_atomic_frame_fragment(stripped) {
        return true;
    }
    // Torn single-line JSON quarantined by `prepare_append_boundary`:
    // skip it if the quarantined payload is not valid JSON.
    let payload = atomic_frame_payload(stripped).unwrap_or(stripped);
    serde_json::from_slice::<serde_json::Value>(payload).is_err()
}

/// Parses only the journal bytes appended after `offset`. Returns `None`
/// when the offset does not land on a line boundary or the tail is not
/// fully classifiable, so callers can fall back to a strict full read that
/// reports canonical line numbers.
fn read_tail<T: DeserializeOwned>(path: &Path, offset: u64) -> io::Result<Option<Vec<T>>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len <= offset {
        return Ok(None);
    }
    if offset > 0 {
        file.seek(SeekFrom::Start(offset - 1))?;
        let mut boundary = [0u8; 1];
        file.read_exact(&mut boundary)?;
        if boundary[0] != b'\n' {
            return Ok(None);
        }
    }
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    let count = data.split(|byte| *byte == b'\n').count();
    let mut values = Vec::new();
    for (index, line) in data.split(|byte| *byte == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let is_physical_eof = index == count - 1 && !data.ends_with(b"\n");
        let payload = atomic_frame_payload(line).unwrap_or(line);
        match serde_json::from_slice(payload) {
            Ok(value) => values.push(value),
            Err(_error) if is_recoverable_journal_fragment(line, is_physical_eof) => {
                tracing::warn!(session_file = %path.display(), tail_line = index + 1,
                    "skipping incomplete session journal frame; interruption cause unknown");
                continue;
            }
            // Unrecoverable content: let the full strict read produce the
            // canonical InvalidData error with file-relative line numbers.
            Err(_) => return Ok(None),
        }
    }
    Ok(Some(values))
}

fn read_strict<T: DeserializeOwned>(path: &Path) -> io::Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    // A torn write can end inside UTF-8; decode records independently.
    let data = fs::read(path)?;
    let count = data.split(|byte| *byte == b'\n').count();
    let mut values = Vec::new();
    for (index, line) in data.split(|byte| *byte == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let is_physical_eof = index == count - 1 && !data.ends_with(b"\n");
        let payload = atomic_frame_payload(line).unwrap_or(line);
        match serde_json::from_slice(payload) {
            Ok(value) => values.push(value),
            Err(_error) if is_recoverable_journal_fragment(line, is_physical_eof) => {
                tracing::warn!(session_file = %path.display(), line = index + 1,
                    "skipping incomplete session journal frame; interruption cause unknown");
                continue
            }
            Err(error) => return Err(invalid_line(path, index + 1, error)),
        }
    }
    Ok(values)
}

fn invalid_line(path: &Path, line: usize, error: impl std::fmt::Display) -> io::Error {
    tracing::error!(session_file = %path.display(), line, %error, "invalid session journal record");
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
        ContextSnapshot, ContextSnapshotLoadOutcome, EffectAction, HarnessEventHub, JsonlStore,
        Record, Reducer, SessionStore, TraceString,
    };
    use std::path::Path;
    use threadlane_protocol::AgentMessage;

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

    #[test]
    fn indexed_branch_tracks_append_atomic_refresh_and_reload() {
        fn ids(store: &JsonlStore, leaf: Option<&str>, limit: usize) -> Vec<String> {
            store
                .branch(leaf, limit)
                .into_iter()
                .map(|entry| entry.id)
                .collect()
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("branches.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        assert!(store.branch(None, usize::MAX).is_empty());
        store.append_entry(user_entry("root", "main")).unwrap();
        let mut next = user_entry("next", "main");
        next.parent_id = Some("root".into());
        store.append_entry(next).unwrap();
        let mut fork = user_entry("fork", "child");
        fork.parent_id = Some("root".into());
        store
            .append_actions_atomically(&[EffectAction::AppendEntry { entry: fork }])
            .unwrap();
        let assert_branches = |store: &JsonlStore| {
            assert_eq!(ids(store, Some("next"), usize::MAX), ["root", "next"]);
            assert_eq!(ids(store, Some("fork"), usize::MAX), ["root", "fork"]);
            assert_eq!(ids(store, None, 1), ["fork"]);
            assert!(store.branch(Some("missing"), usize::MAX).is_empty());
            assert!(store.branch(Some("next"), 0).is_empty());
        };
        assert_branches(&store);
        assert_branches(&JsonlStore::open_read_only(&path).unwrap());
        let mut observer = JsonlStore::open_read_only(&path).unwrap();
        let mut tail = user_entry("tail", "main");
        tail.parent_id = Some("next".into());
        store.append_entry(tail).unwrap();
        observer.ensure_fresh().unwrap();
        assert_eq!(ids(&observer, Some("tail"), 2), ["next", "tail"]);
        assert_eq!(ids(&observer, None, usize::MAX), ["root", "next", "tail"]);
    }

    #[test]
    fn operation_wall_time_survives_single_and_atomic_commits_and_legacy_loads() {
        use crate::harness::{EffectAction, OperationIntent, OperationOutcome};
        let start = Record::OperationStarted {
            id: "timed-run".into(), seq: 1, lane: "main".into(), timestamp: 7,
            wall_time_ms: None, source_leaf_id: None, intent: OperationIntent::Run,
        };
        let finish = Record::OperationFinished {
            id: "timed-finish".into(), seq: 2, lane: "main".into(), timestamp: 8,
            wall_time_ms: None, run_id: "timed-run".into(),
            outcome: OperationOutcome::Completed, error: None,
        };
        let now = || std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        for atomic in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("timing.jsonl");
            let mut store = JsonlStore::open(&path).unwrap();
            let before = now();
            if atomic {
                store.append_actions_atomically(&[
                    EffectAction::AppendRecord { id: "start-action".into(), record: start.clone() },
                    EffectAction::AppendRecord { id: "finish-action".into(), record: finish.clone() },
                ]).unwrap();
            } else {
                store.append_record(start.clone()).unwrap();
                store.append_record(finish.clone()).unwrap();
            }
            let after = now();
            let stored = store.records().to_vec();
            drop(store);
            let restored = JsonlStore::open_read_only(&path).unwrap();
            assert_eq!(restored.records(), stored);
            for (record, logical) in restored.records().iter().zip([7, 8]) {
                let (Record::OperationStarted { timestamp, wall_time_ms, .. }
                    | Record::OperationFinished { timestamp, wall_time_ms, .. }) = record else {
                    panic!("expected operation record");
                };
                assert_eq!(*timestamp, logical);
                assert!((before..=after).contains(&wall_time_ms.unwrap()));
            }
        }
        for legacy in [start, finish] {
            let json = serde_json::to_string(&legacy).unwrap();
            assert!(!json.contains("wall_time_ms"));
            assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), legacy);
        }
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
                    images: Vec::new(),
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
                    images: Vec::new(),
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
        // NOTE: crash-critical provider/stream records (request boundaries,
        // run context, checkpoints) use full sync so a crash cannot leave a
        // torn tail that bricks the session; pure observations stay on Data.
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
            wall_time_ms: None,
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

        assert_eq!(checkpoint.sync_policy(), SyncPolicy::All);
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
        // Quarantined torn single-line tails (invalid JSON + sentinel) are
        // now recovered by skipping the torn line instead of bricking the
        // session.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("torn-suffix.jsonl");
        let line = format!("junk{TORN_EOF_SENTINEL}\n");
        std::fs::write(&path, line.as_bytes()).unwrap();
        let store = JsonlStore::open(&path).unwrap();
        assert!(store.entries().is_empty() && store.records().is_empty());
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
        // Quarantined torn tails are skipped, yielding an empty page.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript-torn-suffix.jsonl");
        let line = format!("junk{TORN_EOF_SENTINEL}\n");
        std::fs::write(&path, line.as_bytes()).unwrap();
        let page = read_transcript_page(&path, None, 1).unwrap();
        assert!(page.messages().is_empty());
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
                wall_time_ms: None,
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
                    images: Vec::new(),
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
                wall_time_ms: None,
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

    /// The frame this store just serialized is applied to in-memory state
    /// directly; the journal is not re-parsed after its own commits.
    #[test]
    fn atomic_commit_folds_new_frame_without_full_reload() {
        use crate::harness::EffectAction;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atomic-fold.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store.append_entry(user_entry("seed", "main")).unwrap();
        super::FULL_RELOAD_COUNT.with(|count| count.set(0));

        store
            .append_actions_atomically(&[
                EffectAction::AppendEntry {
                    entry: user_entry("msg-1", "main"),
                },
                EffectAction::AppendRecord {
                    id: "fact-1".into(),
                    record: Record::FactSet {
                        id: "fact-1".into(),
                        seq: 0,
                        lane: "main".into(),
                        timestamp: 1,
                        run_id: None,
                        key: "model".into(),
                        value: "test-model".into(),
                    },
                },
                EffectAction::AppendEntry {
                    entry: crate::harness::Entry::new(
                        "asst-1",
                        Some("msg-1".to_owned()),
                        "main",
                        0,
                        0,
                        assistant("hi"),
                        false,
                    ),
                },
            ])
            .unwrap();

        super::FULL_RELOAD_COUNT.with(|count| assert_eq!(count.get(), 0));
        let reloaded = JsonlStore::open_read_only(&path).unwrap();
        assert_eq!(store.entries(), reloaded.entries());
        assert_eq!(store.records(), reloaded.records());
        assert_eq!(store.reduced_state(), reloaded.reduced_state());
        assert_eq!(store.preferred_leaf("main"), reloaded.preferred_leaf("main"));
    }

    /// A journal grown by another writer is merged by parsing only the new
    /// tail; the result is byte-identical to a full reload.
    #[test]
    fn foreign_appends_fold_tail_without_full_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tail-fold.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store.append_entry(user_entry("seed", "main")).unwrap();
        let mut peer = JsonlStore::open(&path).unwrap();
        super::FULL_RELOAD_COUNT.with(|count| count.set(0));

        peer.append_entry(user_entry("peer-1", "main")).unwrap();
        peer.append_record(Record::FactSet {
            id: "peer-fact".into(),
            seq: peer.next_sequence(),
            lane: "main".into(),
            timestamp: 1,
            run_id: None,
            key: "name".into(),
            value: "peer".into(),
        })
        .unwrap();
        store.refresh().unwrap();
        // A second growth folds on top of the first tail-apply.
        peer.append_entry(user_entry("peer-2", "lane-2")).unwrap();
        peer.append_entry(user_entry("peer-3", "main")).unwrap();
        store.refresh().unwrap();

        super::FULL_RELOAD_COUNT.with(|count| assert_eq!(count.get(), 0));
        let reloaded = JsonlStore::open_read_only(&path).unwrap();
        assert_eq!(store.entries(), reloaded.entries());
        assert_eq!(store.records(), reloaded.records());
        assert_eq!(store.reduced_state(), reloaded.reduced_state());
        assert_eq!(store.preferred_leaf("main"), reloaded.preferred_leaf("main"));
    }

    /// A tail numbered inside committed history cannot be folded; the store
    /// falls back to the canonical full reload, which surfaces the
    /// duplicate-sequence error the strict path always reported.
    #[test]
    fn unclassifiable_tail_falls_back_to_full_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fallback.jsonl");
        let mut store = JsonlStore::open(&path).unwrap();
        store.append_entry(user_entry("seed", "main")).unwrap();
        super::FULL_RELOAD_COUNT.with(|count| count.set(0));

        let mut low_seq = user_entry("low-seq", "main");
        low_seq.seq = 1;
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{}\n", serde_json::to_string(&low_seq).unwrap()).as_bytes())
            .unwrap();
        store.refresh().unwrap_err();
        super::FULL_RELOAD_COUNT.with(|count| assert_eq!(count.get(), 1));
    }

    fn legacy_pinned_store(path: &Path) -> JsonlStore {
        let mut store = JsonlStore::open(path).unwrap();
        store.append_entry(user_entry("first", "main")).unwrap();
        store.append_entry(user_entry("second", "main")).unwrap();
        use std::io::Write;
        // Legacy writers can pin an older node via session_metadata; parsed
        // last, it wins over the implicit latest-entry leaf.
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"{\"type\":\"session_metadata\",\"active_node_id\":\"first\",\"title_attempted\":true}\n")
            .unwrap();
        JsonlStore::open(path).unwrap()
    }

    fn main_leaf(store: &JsonlStore) -> Option<String> {
        store
            .reduced_state()
            .lanes
            .iter()
            .find(|lane| lane.name == "main")
            .and_then(|lane| lane.leaf_id.clone())
    }

    /// The `active_node_id` pin is a whole-stream legacy rule: the first
    /// `OperationStarted` must re-project the main leaf exactly as a fresh
    /// build does, whether the operation lands via our atomic commit or a
    /// foreign append folded through the tail path.
    #[test]
    fn first_v2_operation_unpins_legacy_active_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2-transition-atomic.jsonl");
        let mut store = legacy_pinned_store(&path);
        assert_eq!(main_leaf(&store).as_deref(), Some("first"));

        store
            .append_actions_atomically(&[EffectAction::AppendRecord {
                id: "run-sub".into(),
                record: Record::OperationStarted {
                    id: "run-sub".into(),
                    seq: store.next_sequence(),
                    lane: "subagent".into(),
                    timestamp: 1,
                    wall_time_ms: None,
                    source_leaf_id: Some("first".into()),
                    intent: crate::harness::OperationIntent::Run,
                },
            }])
            .unwrap();

        assert_eq!(main_leaf(&store).as_deref(), Some("second"));
        let reloaded = JsonlStore::open_read_only(&path).unwrap();
        assert_eq!(store.reduced_state(), reloaded.reduced_state());
    }

    #[test]
    fn foreign_v2_operation_unpins_legacy_active_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2-transition-tail.jsonl");
        let mut store = legacy_pinned_store(&path);
        let mut peer = JsonlStore::open(&path).unwrap();
        assert_eq!(main_leaf(&store).as_deref(), Some("first"));

        peer.append_record(Record::OperationStarted {
            id: "run-peer".into(),
            seq: peer.next_sequence(),
            lane: "lane-2".into(),
            timestamp: 1,
            wall_time_ms: None,
            source_leaf_id: Some("first".into()),
            intent: crate::harness::OperationIntent::Run,
        })
        .unwrap();
        store.refresh().unwrap();

        assert_eq!(main_leaf(&store).as_deref(), Some("second"));
        let reloaded = JsonlStore::open_read_only(&path).unwrap();
        assert_eq!(store.reduced_state(), reloaded.reduced_state());
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
    fn ordinary_tail_recovery_rejects_completed_corruption_and_schema_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.jsonl");
        for bytes in [b"{\n".as_slice(), b"{}", b"junk", b"{\n{}\n"] {
            std::fs::write(&path, bytes).unwrap();
            assert!(JsonlStore::open_read_only(&path).is_err(), "{bytes:?}");
            assert!(read_transcript_page(&path, None, 10).is_err(), "{bytes:?}");
        }
        let entry = serde_json::to_vec(&transcript_entry(1, assistant("complete"))).unwrap();
        std::fs::write(&path, entry).unwrap();
        assert_eq!(JsonlStore::open_read_only(&path).unwrap().entries().len(), 1);
        assert_eq!(read_transcript_page(&path, None, 1).unwrap().messages().len(), 1);
    }

    #[test]
    fn terminated_tail_probe_reads_only_one_byte() {
        struct Probe(std::io::Cursor<Vec<u8>>);
        impl std::io::Read for Probe {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                assert_eq!(bytes.len(), 1, "normal probes must not scan the journal");
                std::io::Read::read(&mut self.0, bytes)
            }
        }
        impl std::io::Seek for Probe {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                std::io::Seek::seek(&mut self.0, position)
            }
        }
        let mut bytes = vec![b'x'; 128 * 1024];
        bytes.push(b'\n');
        let len = bytes.len() as u64;
        let mut probe = Probe(std::io::Cursor::new(bytes));
        assert!(super::read_unterminated_tail(&mut probe, len)
            .unwrap()
            .is_none());
        assert!(super::read_unterminated_tail(&mut probe, 0)
            .unwrap()
            .is_none());
    }

    #[test]
    fn long_unterminated_tail_survives_the_next_append() {
        let entry = transcript_entry(1, assistant("result 🦀".repeat(20_000)));
        for atomic in [false, true] {
            let mut encoded = if atomic {
                let frame = AtomicBatchLine {
                    atomic_batch: vec![AtomicBatchItem::Entry(entry.clone())],
                };
                let mut bytes = ATOMIC_FRAME_SENTINEL.as_bytes().to_vec();
                bytes.extend(serde_json::to_vec(&frame).unwrap());
                bytes
            } else {
                serde_json::to_vec(&entry).unwrap()
            };
            assert!(encoded.len() > 64 * 1024);
            for torn in [false, true] {
                if torn {
                    // Include a cut inside UTF-8, well beyond the old probe window.
                    let cut = encoded.iter().rposition(|byte| *byte == 0xa6).unwrap();
                    encoded.truncate(cut);
                }
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("long-tail.jsonl");
                std::fs::write(&path, &encoded).unwrap();
                let mut store = JsonlStore::open(&path).unwrap();
                assert_eq!(store.entries().len(), usize::from(!torn));
                if atomic {
                    store
                        .append_actions_atomically(&[EffectAction::AppendEntry {
                            entry: user_entry("after", "main"),
                        }])
                        .unwrap();
                } else {
                    store.append_entry(user_entry("after", "main")).unwrap();
                }
                drop(store);
                let durable = std::fs::read(&path).unwrap();
                assert!(durable.starts_with(&encoded));
                let reloaded = JsonlStore::open(&path).unwrap();
                assert_eq!(reloaded.entries().len(), 1 + usize::from(!torn));
                if !torn {
                    assert_eq!(reloaded.entries()[0], entry);
                }
                assert_eq!(
                    read_transcript_page(&path, None, 2)
                        .unwrap()
                        .messages()
                        .len(),
                    1 + usize::from(!torn),
                );
            }
        }
    }

    #[test]
    fn every_ordinary_entry_cut_recovers_without_rewriting_history() {
        let encoded = serde_json::to_vec(&transcript_entry(2, assistant("result 🦀"))).unwrap();
        for cut in 1..encoded.len() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("entry-cut.jsonl");
            let mut store = JsonlStore::open(&path).unwrap();
            store.append_entry(user_entry("root", "main")).unwrap();
            drop(store);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.extend_from_slice(&encoded[..cut]);
            std::fs::write(&path, &bytes).unwrap();

            assert_eq!(
                JsonlStore::open_read_only(&path).unwrap().entries().len(),
                1,
                "cut {cut}"
            );
            assert_eq!(
                read_transcript_page(&path, None, 1).unwrap().messages().len(),
                1
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            let mut recovered = JsonlStore::open(&path).unwrap();
            recovered.append_entry(user_entry("after", "main")).unwrap();
            drop(recovered);
            assert!(std::fs::read(&path).unwrap().starts_with(&bytes));
            assert_eq!(JsonlStore::open(&path).unwrap().entries().len(), 2);
            assert_eq!(
                read_transcript_page(&path, None, 2).unwrap().messages().len(),
                2
            );
        }
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

#[cfg(test)]
mod session_line_tests {
    use super::*;
    use threadlane_protocol::AgentMessage;

    /// Pins untagged classification: permissive shapes must never swallow
    /// `session_metadata`/`session_plan` lines, and legacy nodes must land
    /// on `Legacy` (never `Entry`, which would drop the seq default).
    ///
    /// The order is load-bearing: `Entry` precedes `Legacy` because legacy
    /// nodes are the more permissive shape (defaulted timestamp/seq) and
    /// would otherwise swallow real entries; `Known` is disjoint via its
    /// `type` tag and order-independent.
    #[test]
    fn each_line_shape_classifies_exactly_once() {
        let entry = crate::harness::Entry::new(
            "e-1",
            None,
            "main",
            1,
            1,
            AgentMessage::user("hi", Vec::new()),
            false,
        );
        let record = Record::FactSet {
            id: "fact-1".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            run_id: None,
            key: "name".into(),
            value: "demo".into(),
        };
        let lines = [
            serde_json::to_string(&entry).unwrap(),
            serde_json::to_string(&record).unwrap(),
            r#"{"type":"session_metadata","name":"demo","model":"m"}"#.to_string(),
            r#"{"type":"session_plan","items":[]}"#.to_string(),
            format!(
                r#"{{"id":"legacy-1","message":{}}}"#,
                serde_json::to_string(&AgentMessage::user("old", Vec::new())).unwrap()
            ),
        ]
        .join("\n");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("classify.jsonl");
        std::fs::write(&path, lines).unwrap();
        let parsed: Vec<SessionLine> = read_strict(&path).unwrap();
        assert_eq!(parsed.len(), 5);
        assert!(matches!(parsed[0], SessionLine::Entry(_)));
        assert!(matches!(parsed[1], SessionLine::Record(_)));
        assert!(matches!(
            parsed[2],
            SessionLine::Known(KnownSessionRecord::Metadata { .. })
        ));
        assert!(matches!(
            parsed[3],
            SessionLine::Known(KnownSessionRecord::Plan { .. })
        ));
        assert!(matches!(parsed[4], SessionLine::Legacy(_)));
    }
}
