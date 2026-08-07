use super::reducer::{validate_candidate_entry, validate_candidate_record};
use super::store::SessionStore;
use super::types::{Entry, Record, ReduceError};
use libsqlite3_sys as sqlite;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[cfg(unix)]
use std::os::fd::AsRawFd;

#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_NB: i32 = 4;

// ponytail: one process-wide SQLite append gate; replace with per-database
// locks if concurrent database throughput becomes a measured bottleneck.
fn append_gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

struct WriterClaim {
    _file: fs::File,
}

fn writer_claim(path: &Path) -> Result<Arc<WriterClaim>, ReduceError> {
    static CLAIMS: OnceLock<
        Mutex<std::collections::HashMap<std::path::PathBuf, Weak<WriterClaim>>>,
    > = OnceLock::new();
    let claims = CLAIMS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut claims = claims
        .lock()
        .map_err(|error| ReduceError::Storage(error.to_string()))?;
    if let Some(claim) = claims.get(path).and_then(Weak::upgrade) {
        return Ok(claim);
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
        .map_err(storage)?;
    #[cfg(unix)]
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        return Err(storage(std::io::Error::last_os_error()));
    }
    let claim = Arc::new(WriterClaim { _file: file });
    claims.insert(path.to_path_buf(), Arc::downgrade(&claim));
    Ok(claim)
}

pub struct SqliteStore {
    db: *mut sqlite::sqlite3,
    _writer_lock: Arc<WriterClaim>,
    session_id: String,
    parent_session_id: Option<String>,
    entries: Vec<Entry>,
    records: Vec<Record>,
}

unsafe impl Send for SqliteStore {}

impl SqliteStore {
    pub fn open(
        path: impl AsRef<Path>,
        session_id: impl Into<String>,
    ) -> Result<Self, ReduceError> {
        let lock_path = path.as_ref().with_extension("sqlite.lock");
        if let Some(parent) = path
            .as_ref()
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(storage)?;
        }
        let writer_lock = writer_claim(&lock_path)?;
        let path = CString::new(path.as_ref().to_string_lossy().as_bytes())
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        let mut db = ptr::null_mut();
        let rc = unsafe {
            sqlite::sqlite3_open_v2(
                path.as_ptr(),
                &mut db,
                sqlite::SQLITE_OPEN_CREATE
                    | sqlite::SQLITE_OPEN_READWRITE
                    | sqlite::SQLITE_OPEN_FULLMUTEX,
                ptr::null(),
            )
        };
        if rc != sqlite::SQLITE_OK {
            let error = db_error(db, rc);
            if !db.is_null() {
                unsafe { sqlite::sqlite3_close(db) };
            }
            return Err(ReduceError::Storage(error));
        }
        unsafe {
            sqlite::sqlite3_busy_timeout(db, 5_000);
        }
        let session_id = session_id.into();
        let result = (|| {
            exec(db, "PRAGMA foreign_keys = ON")?;
            exec(db, "PRAGMA journal_mode = WAL")?;
            exec(db, "CREATE TABLE IF NOT EXISTS harness_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")?;
            exec(db, "CREATE TABLE IF NOT EXISTS harness_entries (id TEXT PRIMARY KEY, seq INTEGER NOT NULL UNIQUE, parent_id TEXT, timestamp INTEGER NOT NULL, terminate INTEGER NOT NULL, payload TEXT NOT NULL)")?;
            exec(db, "CREATE TABLE IF NOT EXISTS harness_records (id TEXT PRIMARY KEY, seq INTEGER NOT NULL UNIQUE, lane TEXT NOT NULL, payload TEXT NOT NULL)")?;
            exec(db, "CREATE INDEX IF NOT EXISTS harness_entries_parent_seq ON harness_entries(parent_id, seq)")?;
            exec(
                db,
                "CREATE INDEX IF NOT EXISTS harness_records_lane_seq ON harness_records(lane, seq)",
            )?;
            exec(
                db,
                "CREATE INDEX IF NOT EXISTS harness_records_type_seq ON harness_records(json_extract(payload, '$.type'), seq)",
            )?;
            exec(
                db,
                "CREATE INDEX IF NOT EXISTS harness_records_run_seq ON harness_records(json_extract(payload, '$.run_id'), seq)",
            )?;
            exec(
                db,
                "CREATE INDEX IF NOT EXISTS harness_entries_parent_id ON harness_entries(parent_id)",
            )?;
            if let Some(existing) = read_meta(db, "session_id")? {
                if existing != session_id {
                    return Err(ReduceError::Storage(format!(
                        "sqlite session id mismatch: expected {session_id}, found {existing}"
                    )));
                }
            }
            set_meta(db, "session_id", &session_id)?;
            let parent_session_id = read_meta(db, "parent_session_id")?;
            let entries = load_rows(db, "SELECT payload FROM harness_entries ORDER BY seq")?;
            let records = load_rows(db, "SELECT payload FROM harness_records ORDER BY seq")?;
            Ok((parent_session_id, entries, records))
        })();
        match result {
            Ok((parent_session_id, entries, records)) => {
                let store = Self {
                    db,
                    _writer_lock: writer_lock,
                    session_id,
                    parent_session_id,
                    entries,
                    records,
                };
                super::Reducer::reduce(&store)?;
                Ok(store)
            }
            Err(error) => {
                unsafe { sqlite::sqlite3_close(db) };
                Err(error)
            }
        }
    }

    pub fn path_exists(path: impl AsRef<Path>) -> bool {
        path.as_ref().exists()
    }

    pub fn parent_session_id(&self) -> Option<&str> {
        self.parent_session_id.as_deref()
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
        session_id: impl Into<String>,
        included: &HashSet<String>,
    ) -> Result<Self, ReduceError> {
        let mut fork = Self::open(path, session_id)?;
        set_meta(fork.db, "parent_session_id", &self.session_id)?;
        fork.parent_session_id = Some(self.session_id.clone());
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
            entry.seq = fork.next_seq();
            fork.append_entry(entry)?;
        }
        // Facts are configuration, not operation history. Preserve them in
        // forks while leaving attempts, queues, and usage behind.
        for source in self
            .records
            .iter()
            .filter(|record| matches!(record, Record::FactSet { .. }))
        {
            fork.append_record(source.clone().with_seq(fork.next_seq()))?;
        }
        Ok(fork)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn records(&self) -> &[Record] {
        &self.records
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

    fn append_payload<T: Serialize>(
        &self,
        sql: &str,
        id: &str,
        seq: u64,
        lane: Option<&str>,
        payload: &T,
        parent_id: Option<&str>,
        timestamp: Option<u64>,
        terminate: Option<bool>,
    ) -> Result<(), ReduceError> {
        exec(self.db, "BEGIN IMMEDIATE")?;
        let result = (|| {
            let statement = prepare(self.db, sql)?;
            bind_text(statement, 1, id)?;
            bind_int(statement, 2, seq)?;
            let mut index = 3;
            if let Some(lane) = lane {
                bind_text(statement, index, lane)?;
                index += 1;
            }
            if let Some(parent_id) = parent_id {
                bind_text(statement, index, parent_id)?;
            } else if timestamp.is_some() {
                let rc = unsafe { sqlite::sqlite3_bind_null(statement, index) };
                if rc != sqlite::SQLITE_OK {
                    return Err(ReduceError::Storage(format!("sqlite bind null: {rc}")));
                }
            }
            if let Some(timestamp) = timestamp {
                index += 1;
                bind_int(statement, index, timestamp)?;
                index += 1;
                bind_int(statement, index, terminate.unwrap_or(false) as u64)?;
                index += 1;
            }
            bind_text(
                statement,
                index,
                &serde_json::to_string(payload).map_err(storage)?,
            )?;
            let step_result = step_done(statement, self.db);
            unsafe {
                sqlite::sqlite3_finalize(statement);
            }
            step_result?;
            exec(self.db, "COMMIT")
        })();
        if result.is_err() {
            let _ = exec(self.db, "ROLLBACK");
        }
        result
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

impl SessionStore for SqliteStore {
    fn session_id(&self) -> &str {
        &self.session_id
    }
    fn entries(&self) -> &[Entry] {
        &self.entries
    }
    fn records(&self) -> &[Record] {
        &self.records
    }

    fn append_entry(&mut self, mut entry: Entry) -> Result<(), ReduceError> {
        let _gate = append_gate()
            .lock()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        for _ in 0..3 {
            self.reload()?;
            entry.seq = self.next_seq();
            validate_entry(&self.entries, &self.records, &entry, entry.seq)?;
            validate_candidate_entry(self, &entry)?;
            match self.append_payload(
                "INSERT INTO harness_entries(id, seq, parent_id, timestamp, terminate, payload) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                &entry.id, entry.seq, None, &entry, entry.parent_id.as_deref(), Some(entry.timestamp), Some(entry.terminate),
            ) {
                Ok(()) => return self.reload(),
                Err(error) if is_sequence_conflict(&error) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(ReduceError::Storage(
            "sqlite sequence allocation conflicted repeatedly".into(),
        ))
    }

    fn append_record(&mut self, mut record: Record) -> Result<(), ReduceError> {
        let _gate = append_gate()
            .lock()
            .map_err(|error| ReduceError::Storage(error.to_string()))?;
        for _ in 0..3 {
            self.reload()?;
            record = record.with_seq(self.next_seq());
            validate_record(&self.entries, &self.records, &record, record.seq())?;
            validate_candidate_record(self, &record)?;
            match self.append_payload(
                "INSERT INTO harness_records(id, seq, lane, payload) VALUES(?1, ?2, ?3, ?4)",
                record.id(),
                record.seq(),
                Some(record.lane()),
                &record,
                None,
                None,
                None,
            ) {
                Ok(()) => return self.reload(),
                Err(error) if is_sequence_conflict(&error) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(ReduceError::Storage(
            "sqlite sequence allocation conflicted repeatedly".into(),
        ))
    }
}

impl SqliteStore {
    fn reload(&mut self) -> Result<(), ReduceError> {
        self.entries = load_rows(self.db, "SELECT payload FROM harness_entries ORDER BY seq")?;
        self.records = load_rows(self.db, "SELECT payload FROM harness_records ORDER BY seq")?;
        super::Reducer::reduce(self).map(|_| ())
    }
}

fn is_sequence_conflict(error: &ReduceError) -> bool {
    match error {
        ReduceError::Storage(message) => {
            message.contains("harness_entries.seq") || message.contains("harness_records.seq")
        }
        _ => false,
    }
}

impl Drop for SqliteStore {
    fn drop(&mut self) {
        if !self.db.is_null() {
            unsafe {
                sqlite::sqlite3_close(self.db);
            }
        }
    }
}

fn validate_entry(
    entries: &[Entry],
    records: &[Record],
    entry: &Entry,
    next_seq: u64,
) -> Result<(), ReduceError> {
    if entry.id.trim().is_empty()
        || entries.iter().any(|candidate| candidate.id == entry.id)
        || records.iter().any(|record| record.id() == entry.id)
    {
        return Err(ReduceError::DuplicateId(entry.id.clone()));
    }
    if entry.lane.trim().is_empty() {
        return Err(ReduceError::InvalidLane(entry.lane.clone()));
    }
    if let Some(parent) = &entry.parent_id {
        if !entries.iter().any(|candidate| &candidate.id == parent) {
            return Err(ReduceError::MissingParent(parent.clone()));
        }
    }
    if entry.seq < next_seq {
        return Err(ReduceError::NonMonotonicSequence {
            previous: next_seq - 1,
            current: entry.seq,
        });
    }
    Ok(())
}

fn validate_record(
    entries: &[Entry],
    records: &[Record],
    record: &Record,
    next_seq: u64,
) -> Result<(), ReduceError> {
    if record.id().trim().is_empty()
        || entries.iter().any(|entry| entry.id == record.id())
        || records.iter().any(|current| current.id() == record.id())
    {
        return Err(ReduceError::DuplicateId(record.id().into()));
    }
    if record.lane().trim().is_empty() {
        return Err(ReduceError::InvalidLane(record.lane().into()));
    }
    if record.seq() < next_seq {
        return Err(ReduceError::NonMonotonicSequence {
            previous: next_seq - 1,
            current: record.seq(),
        });
    }
    Ok(())
}

fn storage(error: impl std::fmt::Display) -> ReduceError {
    ReduceError::Storage(error.to_string())
}

fn exec(db: *mut sqlite::sqlite3, sql: &str) -> Result<(), ReduceError> {
    let sql = CString::new(sql).map_err(storage)?;
    let rc =
        unsafe { sqlite::sqlite3_exec(db, sql.as_ptr(), None, ptr::null_mut(), ptr::null_mut()) };
    if rc == sqlite::SQLITE_OK {
        Ok(())
    } else {
        Err(ReduceError::Storage(db_error(db, rc)))
    }
}

fn set_meta(db: *mut sqlite::sqlite3, key: &str, value: &str) -> Result<(), ReduceError> {
    let statement = prepare(db, "INSERT INTO harness_meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value")?;
    bind_text(statement, 1, key)?;
    bind_text(statement, 2, value)?;
    let result = step_done(statement, db);
    unsafe {
        sqlite::sqlite3_finalize(statement);
    }
    result
}

fn read_meta(db: *mut sqlite::sqlite3, key: &str) -> Result<Option<String>, ReduceError> {
    let statement = prepare(db, "SELECT value FROM harness_meta WHERE key = ?1")?;
    bind_text(statement, 1, key)?;
    let rc = unsafe { sqlite::sqlite3_step(statement) };
    let value = if rc == sqlite::SQLITE_ROW {
        let text = unsafe { sqlite::sqlite3_column_text(statement, 0) };
        (!text.is_null()).then(|| {
            unsafe { CStr::from_ptr(text.cast()) }
                .to_string_lossy()
                .into_owned()
        })
    } else if rc == sqlite::SQLITE_DONE {
        None
    } else {
        let error = db_error(db, rc);
        unsafe { sqlite::sqlite3_finalize(statement) };
        return Err(ReduceError::Storage(error));
    };
    unsafe { sqlite::sqlite3_finalize(statement) };
    Ok(value)
}

fn prepare(db: *mut sqlite::sqlite3, sql: &str) -> Result<*mut sqlite::sqlite3_stmt, ReduceError> {
    let sql = CString::new(sql).map_err(storage)?;
    let mut statement = ptr::null_mut();
    let rc = unsafe {
        sqlite::sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut statement, ptr::null_mut())
    };
    if rc == sqlite::SQLITE_OK {
        Ok(statement)
    } else {
        Err(ReduceError::Storage(db_error(db, rc)))
    }
}

fn bind_text(
    statement: *mut sqlite::sqlite3_stmt,
    index: i32,
    value: &str,
) -> Result<(), ReduceError> {
    let value = CString::new(value).map_err(storage)?;
    let rc = unsafe {
        sqlite::sqlite3_bind_text(
            statement,
            index,
            value.as_ptr(),
            -1,
            sqlite::SQLITE_TRANSIENT(),
        )
    };
    if rc == sqlite::SQLITE_OK {
        Ok(())
    } else {
        Err(ReduceError::Storage(format!("sqlite bind text: {rc}")))
    }
}

fn bind_int(
    statement: *mut sqlite::sqlite3_stmt,
    index: i32,
    value: u64,
) -> Result<(), ReduceError> {
    let rc = unsafe { sqlite::sqlite3_bind_int64(statement, index, value as i64) };
    if rc == sqlite::SQLITE_OK {
        Ok(())
    } else {
        Err(ReduceError::Storage(format!("sqlite bind integer: {rc}")))
    }
}

fn step_done(
    statement: *mut sqlite::sqlite3_stmt,
    db: *mut sqlite::sqlite3,
) -> Result<(), ReduceError> {
    let rc = unsafe { sqlite::sqlite3_step(statement) };
    if rc == sqlite::SQLITE_DONE {
        Ok(())
    } else {
        Err(ReduceError::Storage(db_error(db, rc)))
    }
}

fn load_rows<T: DeserializeOwned>(
    db: *mut sqlite::sqlite3,
    sql: &str,
) -> Result<Vec<T>, ReduceError> {
    let statement = prepare(db, sql)?;
    let mut values = Vec::new();
    loop {
        let rc = unsafe { sqlite::sqlite3_step(statement) };
        if rc == sqlite::SQLITE_DONE {
            break;
        }
        if rc != sqlite::SQLITE_ROW {
            let error = db_error(db, rc);
            unsafe {
                sqlite::sqlite3_finalize(statement);
            }
            return Err(ReduceError::Storage(error));
        }
        let text = unsafe { sqlite::sqlite3_column_text(statement, 0) };
        if text.is_null() {
            unsafe {
                sqlite::sqlite3_finalize(statement);
            }
            return Err(ReduceError::Storage(
                "sqlite returned a null payload".into(),
            ));
        }
        let payload = unsafe { CStr::from_ptr(text.cast()).to_bytes() };
        values.push(serde_json::from_slice(payload).map_err(storage)?);
    }
    unsafe {
        sqlite::sqlite3_finalize(statement);
    }
    Ok(values)
}

fn db_error(db: *mut sqlite::sqlite3, rc: i32) -> String {
    if db.is_null() {
        return format!("sqlite error {rc}");
    }
    unsafe {
        format!(
            "sqlite error {rc}: {}",
            CStr::from_ptr(sqlite::sqlite3_errmsg(db)).to_string_lossy()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_a_record_type_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("session.sqlite"), "session").unwrap();
        let statement = prepare(
            store.db,
            "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'harness_records_type_seq'",
        )
        .unwrap();
        let result = unsafe { sqlite::sqlite3_step(statement) };
        unsafe { sqlite::sqlite3_finalize(statement) };
        assert_eq!(result, sqlite::SQLITE_ROW);
    }
}
