//! [`MemoryStore`] on the hub database (Plan.md §8). Every write is versioned.

use async_trait::async_trait;
use threadlane_core::ids::RunId;
use threadlane_core::model::{MemoryFile, MemoryWrite};
use threadlane_core::store::{MemoryStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::row::{Cols, opt_raw, params};

const VERSION_FIELDS: &str =
    "v.id, v.path, v.content, v.version, v.written_by_run, v.deleted, v.written_at";

fn decode_file(row: &Row) -> StoreResult<MemoryFile> {
    let c = Cols(row);
    Ok(MemoryFile {
        id: c.id(0)?,
        path: c.text(1)?,
        content: c.text(2)?,
        version: c.i64(3)?,
        written_by_run: c.opt_id(4)?,
        deleted: c.bool(5)?,
        written_at: c.ts(6)?,
    })
}

/// Rejects paths that could escape the memory root or are not canonical.
fn validate_path(path: &str) -> StoreResult<()> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(StoreError::Invalid(format!("memory path {path:?}")));
    }
    Ok(())
}

async fn write_version(
    conn: &Connection,
    path: &str,
    content: &str,
    deleted: bool,
    run: Option<RunId>,
    now: Timestamp,
) -> StoreResult<MemoryFile> {
    let current: Option<i64> = query_opt(
        conn,
        "SELECT version FROM memory_files WHERE path = ?1",
        params![path],
        |row| Cols(row).i64(0),
    )
    .await?;
    let version = current.unwrap_or(0) + 1;
    let run_param = opt_raw(run, |id| id.get());
    exec(
        conn,
        "INSERT INTO memory_files (path, version, content, deleted, written_by_run, written_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (path) DO UPDATE SET version = excluded.version, content = excluded.content,
             deleted = excluded.deleted, written_by_run = excluded.written_by_run,
             written_at = excluded.written_at",
        params![
            path,
            version,
            content,
            deleted,
            run_param.clone(),
            now.as_millis()
        ],
    )
    .await?;
    let id: i64 = query_one(
        conn,
        "INSERT INTO memory_versions (path, version, content, deleted, written_by_run, written_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
        params![path, version, content, deleted, run_param, now.as_millis()],
        |row| Cols(row).i64(0),
    )
    .await?;
    Ok(MemoryFile {
        id: id.into(),
        path: path.to_owned(),
        content: content.to_owned(),
        version,
        written_by_run: run,
        deleted,
        written_at: now,
    })
}

#[async_trait]
impl MemoryStore for HubDb {
    async fn read(&self, path: &str) -> StoreResult<Option<MemoryFile>> {
        validate_path(path)?;
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!(
                "SELECT {VERSION_FIELDS} FROM memory_files f
                 JOIN memory_versions v ON v.path = f.path AND v.version = f.version
                 WHERE f.path = ?1 AND f.deleted = 0"
            ),
            params![path],
            decode_file,
        )
        .await
    }

    async fn write(&self, write: &MemoryWrite) -> StoreResult<MemoryFile> {
        validate_path(&write.path)?;
        let tx = self.db.begin().await?;
        let file = write_version(
            tx.conn(),
            &write.path,
            &write.content,
            false,
            write.written_by_run,
            write.written_at,
        )
        .await?;
        tx.commit().await?;
        Ok(file)
    }

    async fn delete(&self, path: &str, run: Option<RunId>, now: Timestamp) -> StoreResult<()> {
        validate_path(path)?;
        let tx = self.db.begin().await?;
        let live: Option<i64> = query_opt(
            tx.conn(),
            "SELECT version FROM memory_files WHERE path = ?1 AND deleted = 0",
            params![path],
            |row| Cols(row).i64(0),
        )
        .await?;
        if live.is_none() {
            return Err(StoreError::not_found("memory file", path));
        }
        write_version(tx.conn(), path, "", true, run, now).await?;
        tx.commit().await
    }

    async fn list(&self, prefix: &str) -> StoreResult<Vec<MemoryFile>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {VERSION_FIELDS} FROM memory_files f
                 JOIN memory_versions v ON v.path = f.path AND v.version = f.version
                 WHERE f.deleted = 0 AND substr(f.path, 1, length(?1)) = ?1
                 ORDER BY f.path"
            ),
            params![prefix],
            decode_file,
        )
        .await
    }

    async fn history(&self, path: &str) -> StoreResult<Vec<MemoryFile>> {
        validate_path(path)?;
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {VERSION_FIELDS} FROM memory_versions v WHERE v.path = ?1 ORDER BY v.version"
            ),
            params![path],
            decode_file,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::validate_path;

    #[test]
    fn paths_cannot_escape_the_memory_root() {
        for bad in ["", "/abs", "a/../b", "a//b", "a/", "./a", "a\\b", ".."] {
            assert!(validate_path(bad).is_err(), "{bad:?} should be rejected");
        }
        for good in ["space/instructions.md", "projects/3/notes/INDEX.md"] {
            assert!(validate_path(good).is_ok(), "{good:?} should be accepted");
        }
    }
}
