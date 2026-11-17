//! Schema migrations, tracked with `PRAGMA user_version`.

use threadlane_core::store::{StoreError, StoreResult};

use crate::db::{Db, db_err, query_one};
use crate::row::Cols;

/// Every migration, in order. Index `i` brings the schema to version `i + 1`.
pub(crate) const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_decision_outcome.sql"),
    include_str!("../migrations/0003_memory_call.sql"),
];

/// The schema version this build expects.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

pub(crate) async fn run(db: &Db) -> StoreResult<()> {
    let reader = db.reader()?;
    let current = query_one(&reader, "PRAGMA user_version", (), |row| Cols(row).i64(0)).await?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::Invalid(format!(
            "database schema version {current} is newer than this build ({SCHEMA_VERSION})"
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = index as i64 + 1;
        let tx = db.begin().await?;
        tx.conn().execute_batch(sql).await.map_err(db_err)?;
        tx.conn()
            .execute(&format!("PRAGMA user_version = {version}"), ())
            .await
            .map_err(db_err)?;
        tx.commit().await?;
        tracing::info!(version, "applied schema migration");
    }
    Ok(())
}
