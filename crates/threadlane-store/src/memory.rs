//! [`MemoryStore`] on the hub database (Plan.md §8.3). Every write is
//! versioned with its run and keyed on its call, and the indexes it changes
//! are rewritten in the same transaction.

use async_trait::async_trait;
use threadlane_core::memory::{INDEX_FILE, check, index_for, not_an_index, render_index};
use threadlane_core::model::{MemoryAuthor, MemoryFile};
use threadlane_core::store::{MemoryStore, StoreError, StoreResult};
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::check_lease;
use crate::row::{Cols, params};

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

/// [`check`] as a store error.
fn checked(path: &str, content: &str) -> StoreResult<()> {
    validate_path(path)?;
    check(path, content).map_err(StoreError::Invalid)
}

/// [`not_an_index`] as a store error.
fn unindexed(path: &str) -> StoreResult<()> {
    validate_path(path)?;
    not_an_index(path).map_err(StoreError::Invalid)
}

async fn live_in(conn: &Connection, path: &str) -> StoreResult<Option<MemoryFile>> {
    query_opt(
        conn,
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

async fn list_in(conn: &Connection, prefix: &str) -> StoreResult<Vec<MemoryFile>> {
    query_all(
        conn,
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

/// The live file at `path`, or not found.
async fn existing_in(conn: &Connection, path: &str) -> StoreResult<MemoryFile> {
    live_in(conn, path)
        .await?
        .ok_or_else(|| StoreError::not_found("memory file", path))
}

/// What an earlier change with the same key wrote at `path`, if one did.
async fn replayed_in(
    conn: &Connection,
    by: &MemoryAuthor,
    path: &str,
) -> StoreResult<Option<MemoryFile>> {
    query_opt(
        conn,
        &format!(
            "SELECT {VERSION_FIELDS} FROM memory_versions v
             WHERE v.written_by_call = ?1 AND v.path = ?2 ORDER BY v.version DESC LIMIT 1"
        ),
        params![by.call_key.as_str(), path],
        decode_file,
    )
    .await
}

async fn write_version(
    conn: &Connection,
    by: &MemoryAuthor,
    path: &str,
    content: &str,
    deleted: bool,
) -> StoreResult<MemoryFile> {
    let current: Option<i64> = query_opt(
        conn,
        "SELECT version FROM memory_files WHERE path = ?1",
        params![path],
        |row| Cols(row).i64(0),
    )
    .await?;
    let version = current.unwrap_or(0) + 1;
    let run = by.run_id.get();
    let at = by.at.as_millis();
    exec(
        conn,
        "INSERT INTO memory_files (path, version, content, deleted, written_by_run, written_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (path) DO UPDATE SET version = excluded.version, content = excluded.content,
             deleted = excluded.deleted, written_by_run = excluded.written_by_run,
             written_at = excluded.written_at",
        params![path, version, content, deleted, run, at],
    )
    .await?;
    let id: i64 = query_one(
        conn,
        "INSERT INTO memory_versions
             (path, version, content, deleted, written_by_run, written_at, written_by_call)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
        params![
            path,
            version,
            content,
            deleted,
            run,
            at,
            by.call_key.as_str()
        ],
        |row| Cols(row).i64(0),
    )
    .await?;
    Ok(MemoryFile {
        id: id.into(),
        path: path.to_owned(),
        content: content.to_owned(),
        version,
        written_by_run: Some(by.run_id),
        deleted,
        written_at: by.at,
    })
}

/// Rewrites the index that lists each of `paths`, from the files it lists
/// as they now are: a new version only when its bytes change, a deletion
/// when it lists nothing.
async fn reindex_in(conn: &Connection, by: &MemoryAuthor, paths: &[&str]) -> StoreResult<()> {
    let mut indexes: Vec<String> = paths.iter().filter_map(|path| index_for(path)).collect();
    indexes.dedup();
    for index in indexes {
        let dir = index.strip_suffix(INDEX_FILE).unwrap_or(&index);
        let files: Vec<(String, String)> = list_in(conn, dir)
            .await?
            .into_iter()
            .filter(|file| file.path != index)
            .map(|file| (file.path, file.content))
            .collect();
        let current = live_in(conn, &index).await?;
        if files.is_empty() {
            if current.is_some() {
                write_version(conn, by, &index, "", true).await?;
            }
            continue;
        }
        let text = render_index(&index, &files);
        if current.is_none_or(|file| file.content != text) {
            write_version(conn, by, &index, &text, false).await?;
        }
    }
    Ok(())
}

#[async_trait]
impl MemoryStore for HubDb {
    async fn read(&self, path: &str) -> StoreResult<Option<MemoryFile>> {
        validate_path(path)?;
        let conn = self.db.reader()?;
        live_in(&conn, path).await
    }

    async fn write(&self, by: &MemoryAuthor, path: &str, content: &str) -> StoreResult<MemoryFile> {
        checked(path, content)?;
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, &by.lease).await?;
        if let Some(file) = replayed_in(conn, by, path).await? {
            return Ok(file);
        }
        let file = write_version(conn, by, path, content, false).await?;
        reindex_in(conn, by, &[path]).await?;
        tx.commit().await?;
        Ok(file)
    }

    async fn edit(
        &self,
        by: &MemoryAuthor,
        path: &str,
        old: &str,
        new: &str,
    ) -> StoreResult<MemoryFile> {
        validate_path(path)?;
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, &by.lease).await?;
        if let Some(file) = replayed_in(conn, by, path).await? {
            return Ok(file);
        }
        let current = existing_in(conn, path).await?;
        let found = if old.is_empty() {
            0
        } else {
            current.content.matches(old).count()
        };
        if found != 1 {
            return Err(StoreError::Invalid(if found == 0 {
                format!("the text to replace does not occur in {path}; view it and try again")
            } else {
                format!(
                    "the text to replace occurs {found} times in {path}; include more of what \
                     is around it so that it occurs once"
                )
            }));
        }
        let content = current.content.replacen(old, new, 1);
        checked(path, &content)?;
        let file = write_version(conn, by, path, &content, false).await?;
        reindex_in(conn, by, &[path]).await?;
        tx.commit().await?;
        Ok(file)
    }

    async fn delete(&self, by: &MemoryAuthor, path: &str) -> StoreResult<MemoryFile> {
        unindexed(path)?;
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, &by.lease).await?;
        if let Some(file) = replayed_in(conn, by, path).await? {
            return Ok(file);
        }
        existing_in(conn, path).await?;
        let file = write_version(conn, by, path, "", true).await?;
        reindex_in(conn, by, &[path]).await?;
        tx.commit().await?;
        Ok(file)
    }

    async fn rename(&self, by: &MemoryAuthor, from: &str, to: &str) -> StoreResult<MemoryFile> {
        unindexed(from)?;
        validate_path(to)?;
        if from == to {
            return Err(StoreError::Invalid(format!("{from} is already there")));
        }
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, &by.lease).await?;
        if let Some(file) = replayed_in(conn, by, to).await? {
            return Ok(file);
        }
        let current = existing_in(conn, from).await?;
        // The content must be allowed where it lands.
        checked(to, &current.content)?;
        if live_in(conn, to).await?.is_some() {
            return Err(StoreError::Invalid(format!(
                "{to} already exists; delete it first or pick another name"
            )));
        }
        write_version(conn, by, from, "", true).await?;
        let file = write_version(conn, by, to, &current.content, false).await?;
        reindex_in(conn, by, &[from, to]).await?;
        tx.commit().await?;
        Ok(file)
    }

    async fn list(&self, prefix: &str) -> StoreResult<Vec<MemoryFile>> {
        let conn = self.db.reader()?;
        list_in(&conn, prefix).await
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
