//! The hub's durable stores on Turso (Plan.md §3.2, §18.1).
//!
//! [`HubDb`] implements every store interface in [`threadlane_core::store`]
//! over one database file. It is the only code that touches the database, and
//! the schema uses only SQL that SQLite also accepts, so replacing Turso with
//! SQLite is a change of library behind this crate.
//!
//! Tests open the same implementation on an in-memory database
//! ([`HubDb::open_in_memory`]), which is how the store interfaces are faked
//! (Plan.md §18.1): one implementation, so fakes and production cannot drift.

mod admission;
mod db;
mod decision;
mod inbox;
mod machine;
mod memory;
mod migrate;
mod org;
mod outbox;
mod row;
mod session;
#[cfg(test)]
mod smoke_tests;
mod task;
mod timer;

use std::path::Path;
use std::sync::Arc;

use threadlane_core::model::{Mode, SpaceSettings};
use threadlane_core::store::{StoreError, StoreResult};
use threadlane_core::time::Clock;

use crate::db::{Db, DbLocation, exec, query_all, query_opt};
use crate::row::{Cols, json_text, params};

pub use crate::migrate::SCHEMA_VERSION;
pub use crate::task::{BLOCKED_REASON, NO_GH_REASON, TASK_SOURCE, UNWOKEN_REASON};
pub use crate::timer::TIMER_SOURCE;

/// Name of the built-in project every space has (Plan.md §2.1).
pub const GENERAL_PROJECT_NAME: &str = "General";

/// What a hub database is initialized with on its first start.
#[derive(Debug, Clone, PartialEq)]
pub struct Bootstrap {
    pub space: SpaceSettings,
    /// Parallel task cap for the General project.
    pub general_max_parallel_tasks: u32,
}

impl Default for Bootstrap {
    fn default() -> Self {
        Self {
            space: SpaceSettings::default(),
            general_max_parallel_tasks: 2,
        }
    }
}

/// The hub's stores over one database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct HubDb {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
}

impl HubDb {
    /// Opens (or creates) the database file, migrates it, and makes sure the
    /// space row and the General project exist.
    ///
    /// Only one process may open a hub's database at a time (Plan.md §3.3).
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be opened or migrated.
    pub async fn open(
        path: &Path,
        bootstrap: &Bootstrap,
        clock: Arc<dyn Clock>,
    ) -> StoreResult<Self> {
        Self::open_at(DbLocation::File(path), bootstrap, clock).await
    }

    /// Opens a fresh in-memory database, for tests and simulation.
    ///
    /// # Errors
    ///
    /// Fails only if the schema cannot be applied.
    pub async fn open_in_memory(bootstrap: &Bootstrap, clock: Arc<dyn Clock>) -> StoreResult<Self> {
        Self::open_at(DbLocation::Memory, bootstrap, clock).await
    }

    async fn open_at(
        location: DbLocation<'_>,
        bootstrap: &Bootstrap,
        clock: Arc<dyn Clock>,
    ) -> StoreResult<Self> {
        let db = Arc::new(Db::open(location).await?);
        let hub = Self { db, clock };
        hub.bootstrap(bootstrap).await?;
        Ok(hub)
    }

    async fn bootstrap(&self, bootstrap: &Bootstrap) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let space = &bootstrap.space;
        exec(
            tx.conn(),
            "INSERT INTO space (id, name, mode_ceiling, quiet_hours, channels)
             VALUES (1, ?1, ?2, ?3, ?4) ON CONFLICT (id) DO NOTHING",
            params![
                space.name.as_str(),
                space.mode_ceiling.as_str(),
                space.quiet_hours.as_ref().map(json_text).transpose()?,
                json_text(&space.channels)?,
            ],
        )
        .await?;
        let general = query_opt(
            tx.conn(),
            "SELECT id FROM projects WHERE is_general = 1",
            (),
            |row| Cols(row).i64(0),
        )
        .await?;
        if general.is_none() {
            org::insert_project(
                tx.conn(),
                GENERAL_PROJECT_NAME,
                "Requests not tied to a repository",
                true,
                None,
                bootstrap.general_max_parallel_tasks,
                self.clock.now(),
            )
            .await?;
        }
        tx.commit().await
    }

    /// Runs the database's integrity check. Returns an empty list when the
    /// database is healthy, otherwise the problems it reported.
    ///
    /// # Errors
    ///
    /// Fails if the check itself cannot run.
    pub async fn integrity_check(&self) -> StoreResult<Vec<String>> {
        let reader = self.db.reader()?;
        let lines = query_all(&reader, "PRAGMA integrity_check", (), |row| {
            Cols(row).text(0)
        })
        .await?;
        Ok(lines.into_iter().filter(|line| line != "ok").collect())
    }

    /// The schema version stored in the database.
    ///
    /// # Errors
    ///
    /// Fails if the version cannot be read.
    pub async fn schema_version(&self) -> StoreResult<i64> {
        let reader = self.db.reader()?;
        query_opt(&reader, "PRAGMA user_version", (), |row| Cols(row).i64(0))
            .await?
            .ok_or_else(|| StoreError::Database("PRAGMA user_version returned nothing".into()))
    }

    pub(crate) fn now(&self) -> threadlane_core::time::Timestamp {
        self.clock.now()
    }

    /// The effective ceiling for a project: the stricter of space and project.
    ///
    /// # Errors
    ///
    /// Fails if the project does not exist.
    pub async fn effective_ceiling(
        &self,
        project_id: threadlane_core::ids::ProjectId,
    ) -> StoreResult<Mode> {
        use threadlane_core::store::OrgStore;
        let space = self.space().await?;
        let project = self
            .project(project_id)
            .await?
            .ok_or_else(|| StoreError::not_found("project", project_id))?;
        Ok(project.mode_ceiling.map_or(space.mode_ceiling, |ceiling| {
            ceiling.stricter(space.mode_ceiling)
        }))
    }
}
