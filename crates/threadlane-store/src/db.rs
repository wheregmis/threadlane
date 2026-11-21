//! The hub database: one file, one writer, many readers (Plan.md §18.1).

use std::path::Path;

use threadlane_core::store::{StoreError, StoreResult};
use tokio::sync::{Mutex, MutexGuard};
use turso::{Builder, Connection, Database, IntoParams, Row};

use crate::migrate;

/// Where the hub database lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbLocation<'a> {
    /// A file in the hub's data directory.
    File(&'a Path),
    /// An in-memory database, for tests and simulation (Plan.md §18.1).
    Memory,
}

struct Writer {
    conn: Connection,
    /// Set while a transaction may be open on `conn`. A transaction dropped
    /// without commit (an error, or a cancelled future) leaves this set, and
    /// the next writer rolls it back before beginning its own.
    needs_rollback: bool,
}

/// The open database. Every write goes through one connection, in order;
/// reads use their own connections.
pub(crate) struct Db {
    database: Database,
    writer: Mutex<Writer>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

impl Db {
    pub(crate) async fn open(location: DbLocation<'_>) -> StoreResult<Self> {
        let path = match location {
            DbLocation::File(path) => path
                .to_str()
                .ok_or_else(|| StoreError::Invalid(format!("non-UTF-8 path {}", path.display())))?
                .to_owned(),
            DbLocation::Memory => ":memory:".to_owned(),
        };
        let database = Builder::new_local(&path).build().await.map_err(db_err)?;
        let conn = database.connect().map_err(db_err)?;
        // Full durability: a commit that returned has reached the disk (Plan.md §18.1).
        conn.execute("PRAGMA synchronous = FULL", ())
            .await
            .map_err(db_err)?;
        conn.execute("PRAGMA foreign_keys = ON", ())
            .await
            .map_err(db_err)?;
        let db = Self {
            database,
            writer: Mutex::new(Writer {
                conn,
                needs_rollback: false,
            }),
        };
        migrate::run(&db).await?;
        Ok(db)
    }

    /// A connection for reads. Reads never block the writer.
    pub(crate) fn reader(&self) -> StoreResult<Connection> {
        self.database.connect().map_err(db_err)
    }

    /// Begins the single write transaction. Waits for any earlier writer.
    pub(crate) async fn begin(&self) -> StoreResult<WriteTx<'_>> {
        let mut writer = self.writer.lock().await;
        if writer.needs_rollback {
            // A previous writer was dropped mid-transaction; discard its work.
            if let Err(error) = writer.conn.execute("ROLLBACK", ()).await {
                tracing::debug!(%error, "rollback of abandoned write found no transaction");
            }
            writer.needs_rollback = false;
        }
        writer.needs_rollback = true;
        writer
            .conn
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(db_err)?;
        Ok(WriteTx { writer })
    }
}

/// The open write transaction. Dropping it without [`WriteTx::commit`] rolls
/// it back before the next write begins.
pub(crate) struct WriteTx<'a> {
    writer: MutexGuard<'a, Writer>,
}

impl WriteTx<'_> {
    pub(crate) fn conn(&self) -> &Connection {
        &self.writer.conn
    }

    pub(crate) async fn commit(mut self) -> StoreResult<()> {
        self.writer
            .conn
            .execute("COMMIT", ())
            .await
            .map_err(db_err)?;
        self.writer.needs_rollback = false;
        Ok(())
    }

    pub(crate) async fn rollback(mut self) -> StoreResult<()> {
        self.writer
            .conn
            .execute("ROLLBACK", ())
            .await
            .map_err(db_err)?;
        self.writer.needs_rollback = false;
        Ok(())
    }
}

/// Maps a Turso error onto the store's error kinds.
pub(crate) fn db_err(error: turso::Error) -> StoreError {
    match error {
        turso::Error::Constraint(message) => StoreError::Conflict(message),
        other => StoreError::Database(other.to_string()),
    }
}

/// Runs a statement and returns the number of rows changed.
pub(crate) async fn exec(
    conn: &Connection,
    sql: &str,
    params: impl IntoParams,
) -> StoreResult<u64> {
    conn.execute(sql, params).await.map_err(db_err)
}

/// Runs a query and decodes every row.
pub(crate) async fn query_all<T>(
    conn: &Connection,
    sql: &str,
    params: impl IntoParams,
    decode: impl Fn(&Row) -> StoreResult<T>,
) -> StoreResult<Vec<T>> {
    let mut rows = conn.query(sql, params).await.map_err(db_err)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_err)? {
        out.push(decode(&row)?);
    }
    Ok(out)
}

/// Runs a query and decodes the first row, if any.
pub(crate) async fn query_opt<T>(
    conn: &Connection,
    sql: &str,
    params: impl IntoParams,
    decode: impl Fn(&Row) -> StoreResult<T>,
) -> StoreResult<Option<T>> {
    let mut rows = conn.query(sql, params).await.map_err(db_err)?;
    let first = rows.next().await.map_err(db_err)?;
    // Drain so the statement finishes before the connection is reused.
    while rows.next().await.map_err(db_err)?.is_some() {}
    first.map(|row| decode(&row)).transpose()
}

/// Runs a query that must return exactly one row.
pub(crate) async fn query_one<T>(
    conn: &Connection,
    sql: &str,
    params: impl IntoParams,
    decode: impl Fn(&Row) -> StoreResult<T>,
) -> StoreResult<T> {
    query_opt(conn, sql, params, decode)
        .await?
        .ok_or_else(|| StoreError::Database(format!("query returned no row: {sql}")))
}

// Explicit keys keep stale handles from reaching a different row after a deletion.
macro_rules! next_id_sql {
    ($table:literal) => {
        concat!(
            "(SELECT MAX(COALESCE((SELECT MAX(id) FROM ",
            $table,
            "), 0), COALESCE((SELECT value FROM deletion_floors WHERE name = '",
            $table,
            "'), 0)) + 1)"
        )
    };
}
pub(crate) use next_id_sql;
