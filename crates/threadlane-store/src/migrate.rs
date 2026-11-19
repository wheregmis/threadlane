//! Schema migrations, tracked with `PRAGMA user_version`.

use threadlane_core::store::{StoreError, StoreResult};

use crate::db::{Db, db_err, query_one};
use crate::row::Cols;

/// Every migration, in order. Index `i` brings the schema to version `i + 1`.
pub(crate) const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_decision_outcome.sql"),
    include_str!("../migrations/0003_memory_call.sql"),
    include_str!("../migrations/0004_timer_requested.sql"),
    include_str!("../migrations/0005_standing_rule_uses.sql"),
    include_str!("../migrations/0006_permissions.sql"),
    include_str!("../migrations/0007_delivery.sql"),
    include_str!("../migrations/0008_operations.sql"),
    include_str!("../migrations/0009_dreaming.sql"),
    include_str!("../migrations/0010_project_archive.sql"),
    include_str!("../migrations/0011_project_deletion.sql"),
    include_str!("../migrations/0012_merge_refusals.sql"),
    include_str!("../migrations/0013_space_lead_ask.sql"),
    include_str!("../migrations/0014_agent_deletion.sql"),
    include_str!("../migrations/0015_follow_up_ask.sql"),
    include_str!("../migrations/0016_task_review.sql"),
    include_str!("../migrations/0017_machine_slots.sql"),
    include_str!("../migrations/0018_github_reviews.sql"),
    include_str!("../migrations/0019_decision_reminded.sql"),
    include_str!("../migrations/0020_run_lookup.sql"),
    include_str!("../migrations/0021_agent_skills.sql"),
    include_str!("../migrations/0022_review_recipe.sql"),
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
