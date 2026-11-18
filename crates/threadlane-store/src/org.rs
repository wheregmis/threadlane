//! [`OrgStore`] on the hub database.

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::ids::{AgentId, BindingId, LaneId, ProjectId, TaskId};
use threadlane_core::model::{
    Agent, Binding, LaneKind, Mode, NewAgent, NewBinding, NewProject, NewRepository, Project,
    Repository, SpaceSettings,
};
use threadlane_core::store::{OrgStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::row::{Cols, json_text, params};

const PROJECT_COLUMNS: &str = "p.id, p.name, p.description, p.is_general, p.mode_ceiling,
     p.max_parallel_tasks, p.paused, l.id, p.archived_at
     FROM projects p JOIN lanes l ON l.project_id = p.id AND l.kind = 'lead'";

fn decode_project(row: &Row) -> StoreResult<Project> {
    let c = Cols(row);
    Ok(Project {
        id: c.id(0)?,
        name: c.text(1)?,
        description: c.text(2)?,
        is_general: c.bool(3)?,
        mode_ceiling: c.opt_parse(4)?,
        max_parallel_tasks: c.u32(5)?,
        paused: c.bool(6)?,
        lead_lane_id: c.id(7)?,
        archived_at: c.opt_ts(8)?,
    })
}

/// Refuses new work in an archived project inside the caller's transaction.
pub(crate) async fn require_active_project(
    conn: &Connection,
    project: ProjectId,
) -> StoreResult<()> {
    if let Some(name) = query_opt(
        conn,
        "SELECT name FROM projects WHERE id = ?1 AND archived_at IS NOT NULL",
        params![project.get()],
        |row| Cols(row).text(0),
    )
    .await?
    {
        return Err(StoreError::Invalid(format!(
            "Project {name:?} is archived."
        )));
    }
    Ok(())
}

const REPOSITORY_FIELDS: &str =
    "id, project_id, name, remote_url, default_branch, protected_branches";

fn decode_repository(row: &Row) -> StoreResult<Repository> {
    let c = Cols(row);
    Ok(Repository {
        id: c.id(0)?,
        project_id: c.id(1)?,
        name: c.text(2)?,
        remote_url: c.text(3)?,
        default_branch: c.text(4)?,
        protected_branches: c.de(5)?,
    })
}

const AGENT_COLUMNS: &str =
    "id, name, role, instructions, backends, models, tools, requirements FROM agents";

fn decode_agent(row: &Row) -> StoreResult<Agent> {
    let c = Cols(row);
    Ok(Agent {
        id: c.id(0)?,
        name: c.text(1)?,
        role: c.text(2)?,
        instructions: c.text(3)?,
        backends: c.de(4)?,
        models: c.json(5)?,
        tools: c.json(6)?,
        requirements: c.json(7)?,
    })
}

async fn write_agent(conn: &Connection, agent: &Agent) -> StoreResult<Agent> {
    let changed = exec(
        conn,
        "UPDATE agents SET name = ?2, role = ?3, instructions = ?4, backends = ?5, models = ?6
             WHERE id = ?1 AND deleted_at IS NULL",
        params![
            agent.id.get(),
            agent.name.as_str(),
            agent.role.as_str(),
            agent.instructions.as_str(),
            json_text(&agent.backends)?,
            json_text(&agent.models)?,
        ],
    )
    .await?;
    if changed == 0 {
        return Err(StoreError::NotFound {
            kind: "agent",
            id: agent.id.to_string(),
        });
    }
    query_one(
        conn,
        &format!("SELECT {AGENT_COLUMNS} WHERE id = ?1"),
        params![agent.id.get()],
        decode_agent,
    )
    .await
}

impl HubDb {
    /// Applies an edit to the current agent under the same transaction as its save.
    pub async fn patch_agent(
        &self,
        name: &str,
        apply: impl FnOnce(&mut Agent) -> StoreResult<()>,
    ) -> StoreResult<Agent> {
        let tx = self.db.begin().await?;
        let mut current = query_opt(
            tx.conn(),
            &format!("SELECT {AGENT_COLUMNS} WHERE name = ?1 AND deleted_at IS NULL"),
            params![name],
            decode_agent,
        )
        .await?
        .ok_or_else(|| StoreError::not_found("agent", name))?;
        apply(&mut current)?;
        let updated = write_agent(tx.conn(), &current).await?;
        tx.commit().await?;
        Ok(updated)
    }
}

const BINDING_COLUMNS: &str =
    "id, project_id, agent_id, mode, is_lead, models, max_parallel, overrides FROM bindings";

fn decode_binding(row: &Row) -> StoreResult<Binding> {
    let c = Cols(row);
    Ok(Binding {
        id: c.id(0)?,
        project_id: c.id(1)?,
        agent_id: c.id(2)?,
        mode: c.parse(3)?,
        is_lead: c.bool(4)?,
        models: c.json(5)?,
        max_parallel: c.u32(6)?,
        overrides: c.json(7)?,
    })
}

/// Inserts a project and its lead lane inside the caller's transaction.
pub(crate) async fn insert_project(
    conn: &Connection,
    name: &str,
    description: &str,
    is_general: bool,
    mode_ceiling: Option<Mode>,
    max_parallel_tasks: u32,
    now: Timestamp,
) -> StoreResult<(ProjectId, LaneId)> {
    let project_id: ProjectId = query_one(
        conn,
        concat!("INSERT INTO projects (id, name, description, is_general, mode_ceiling, max_parallel_tasks, created_at)
         VALUES (", crate::db::next_id_sql!("projects"), ", ?1, ?2, ?3, ?4, ?5, ?6) RETURNING id"),
        params![
            name,
            description,
            is_general,
            mode_ceiling.map(Mode::as_str),
            i64::from(max_parallel_tasks),
            now.as_millis(),
        ],
        |row| Cols(row).id(0),
    )
    .await?;
    let lane_id: LaneId = query_one(
        conn,
        concat!(
            "INSERT INTO lanes (id, project_id, kind) VALUES (",
            crate::db::next_id_sql!("lanes"),
            ", ?1, ?2) RETURNING id"
        ),
        params![project_id.get(), LaneKind::Lead.as_str()],
        |row| Cols(row).id(0),
    )
    .await?;
    Ok((project_id, lane_id))
}

#[async_trait]
impl OrgStore for HubDb {
    async fn space(&self) -> StoreResult<SpaceSettings> {
        let conn = self.db.reader()?;
        query_one(
            &conn,
            "SELECT name, mode_ceiling, quiet_hours, channels FROM space WHERE id = 1",
            (),
            |row| {
                let c = Cols(row);
                Ok(SpaceSettings {
                    name: c.text(0)?,
                    mode_ceiling: c.parse(1)?,
                    quiet_hours: c.opt_de(2)?,
                    channels: c.de(3)?,
                })
            },
        )
        .await
    }

    async fn update_space(&self, settings: &SpaceSettings) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "UPDATE space SET name = ?1, mode_ceiling = ?2, quiet_hours = ?3, channels = ?4 WHERE id = 1",
            params![
                settings.name.as_str(),
                settings.mode_ceiling.as_str(),
                settings.quiet_hours.as_ref().map(json_text).transpose()?,
                json_text(&settings.channels)?,
            ],
        )
        .await?;
        tx.commit().await
    }

    async fn create_project(&self, project: &NewProject) -> StoreResult<Project> {
        if project.name.trim().is_empty() {
            return Err(StoreError::Invalid("project name is empty".into()));
        }
        let tx = self.db.begin().await?;
        let (id, _) = insert_project(
            tx.conn(),
            &project.name,
            &project.description,
            false,
            project.mode_ceiling,
            project.max_parallel_tasks,
            self.now(),
        )
        .await?;
        let created = query_one(
            tx.conn(),
            &format!("SELECT {PROJECT_COLUMNS} WHERE p.id = ?1"),
            params![id.get()],
            decode_project,
        )
        .await?;
        tx.commit().await?;
        Ok(created)
    }

    async fn project(&self, id: ProjectId) -> StoreResult<Option<Project>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {PROJECT_COLUMNS} WHERE p.id = ?1"),
            params![id.get()],
            decode_project,
        )
        .await
    }

    async fn projects(&self) -> StoreResult<Vec<Project>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {PROJECT_COLUMNS} ORDER BY p.id"),
            (),
            decode_project,
        )
        .await
    }

    async fn general_project(&self) -> StoreResult<Project> {
        let conn = self.db.reader()?;
        query_one(
            &conn,
            &format!("SELECT {PROJECT_COLUMNS} WHERE p.is_general = 1"),
            (),
            decode_project,
        )
        .await
    }

    async fn set_project_paused(&self, id: ProjectId, paused: bool) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let changed = exec(
            tx.conn(),
            "UPDATE projects SET paused = ?1 WHERE id = ?2",
            params![paused, id.get()],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::not_found("project", id));
        }
        tx.commit().await
    }

    async fn set_project_archived(&self, id: ProjectId, archived: bool) -> StoreResult<Project> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let sql = format!("SELECT {PROJECT_COLUMNS} WHERE p.id = ?1");
        let project = query_one(conn, &sql, params![id.get()], decode_project).await?;
        if archived == project.archived_at.is_some() {
            return Ok(project);
        }
        let now = self.now();
        if archived {
            if project.is_general {
                return Err(StoreError::Invalid(
                    "The General project cannot be archived.".into(),
                ));
            }
            let (running, queued, decisions, active_lane) = query_one(
                conn,
                "SELECT
                     EXISTS (SELECT 1 FROM tasks WHERE project_id = ?1 AND status = 'running'),
                     EXISTS (SELECT 1 FROM tasks WHERE project_id = ?1 AND status = 'queued'),
                     EXISTS (SELECT 1 FROM decisions WHERE project_id = ?1 AND status = 'pending'),
                     EXISTS (SELECT 1 FROM lanes WHERE project_id = ?1 AND lease_expires_at > ?2)",
                params![id.get(), now.as_millis()],
                |row| {
                    let c = Cols(row);
                    Ok((c.bool(0)?, c.bool(1)?, c.bool(2)?, c.bool(3)?))
                },
            )
            .await?;
            let blockers: Vec<_> = [
                (running, "running tasks"),
                (queued, "queued tasks"),
                (decisions, "open decisions"),
                (active_lane && !running, "an active lane"),
            ]
            .into_iter()
            .filter_map(|(blocked, reason)| blocked.then_some(reason))
            .collect();
            if !blockers.is_empty() {
                return Err(StoreError::Invalid(format!(
                    "Project {:?} cannot be archived while it has {}.",
                    project.name,
                    blockers.join(", ")
                )));
            }
        }
        let archived_at = archived.then_some(now);
        exec(
            conn,
            "UPDATE projects SET archived_at = ?1 WHERE id = ?2",
            params![archived_at.map(Timestamp::as_millis), id.get()],
        )
        .await?;
        tx.commit().await?;
        Ok(Project {
            archived_at,
            ..project
        })
    }

    async fn create_repository(&self, repository: &NewRepository) -> StoreResult<Repository> {
        let tx = self.db.begin().await?;
        let created = query_one(
            tx.conn(),
            &format!(
                "INSERT INTO repositories (id, project_id, name, remote_url, default_branch, protected_branches)
                 VALUES ({}, ?1, ?2, ?3, ?4, ?5) RETURNING {REPOSITORY_FIELDS}", crate::db::next_id_sql!("repositories")
            ),
            params![
                repository.project_id.get(),
                repository.name.as_str(),
                repository.remote_url.as_str(),
                repository.default_branch.as_str(),
                json_text(&repository.protected_branches)?,
            ],
            decode_repository,
        )
        .await?;
        tx.commit().await?;
        Ok(created)
    }

    async fn repositories(&self, project_id: ProjectId) -> StoreResult<Vec<Repository>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {REPOSITORY_FIELDS} FROM repositories WHERE project_id = ?1 ORDER BY id"
            ),
            params![project_id.get()],
            decode_repository,
        )
        .await
    }

    async fn repository_by_remote(&self, remote_url: &str) -> StoreResult<Option<Repository>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {REPOSITORY_FIELDS} FROM repositories WHERE remote_url = ?1"),
            params![remote_url],
            decode_repository,
        )
        .await
    }

    async fn create_agent(&self, agent: &NewAgent) -> StoreResult<Agent> {
        let tx = self.db.begin().await?;
        let id: AgentId = query_one(
            tx.conn(),
            "INSERT INTO agents (name, role, instructions, backends, models, tools, requirements)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
            params![
                agent.name.as_str(),
                agent.role.as_str(),
                agent.instructions.as_str(),
                json_text(&agent.backends)?,
                json_text(&agent.models)?,
                json_text(&agent.tools)?,
                json_text(&agent.requirements)?,
            ],
            |row| Cols(row).id(0),
        )
        .await?;
        let created = query_one(
            tx.conn(),
            &format!("SELECT {AGENT_COLUMNS} WHERE id = ?1"),
            params![id.get()],
            decode_agent,
        )
        .await?;
        tx.commit().await?;
        Ok(created)
    }

    async fn agent(&self, id: AgentId) -> StoreResult<Option<Agent>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {AGENT_COLUMNS} WHERE id = ?1"),
            params![id.get()],
            decode_agent,
        )
        .await
    }

    async fn agents(&self) -> StoreResult<Vec<Agent>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {AGENT_COLUMNS} WHERE deleted_at IS NULL ORDER BY id"),
            (),
            decode_agent,
        )
        .await
    }

    async fn all_agents(&self) -> StoreResult<Vec<Agent>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {AGENT_COLUMNS} ORDER BY id"),
            (),
            decode_agent,
        )
        .await
    }

    async fn update_agent(&self, agent: &Agent) -> StoreResult<Agent> {
        let tx = self.db.begin().await?;
        let updated = write_agent(tx.conn(), agent).await?;
        tx.commit().await?;
        Ok(updated)
    }

    async fn delete_agent(&self, id: AgentId) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let name = query_opt(
            conn,
            "SELECT name FROM agents WHERE id = ?1 AND deleted_at IS NULL",
            params![id.get()],
            |row| Cols(row).text(0),
        )
        .await?
        .ok_or_else(|| StoreError::NotFound {
            kind: "agent",
            id: id.to_string(),
        })?;
        let leads: Vec<String> = query_all(
            conn,
            "SELECT p.name FROM bindings b JOIN projects p ON p.id = b.project_id
             WHERE b.agent_id = ?1 AND b.is_lead = 1 ORDER BY p.id",
            params![id.get()],
            |row| Cols(row).text(0),
        )
        .await?;
        if !leads.is_empty() {
            return Err(StoreError::Invalid(format!(
                "{name} is the lead of {}; a lead cannot be deleted",
                leads.join(", ")
            )));
        }
        let live: Vec<TaskId> = query_all(
            conn,
            "SELECT id FROM tasks WHERE agent_id = ?1
             AND status NOT IN ('done', 'failed', 'cancelled') ORDER BY id",
            params![id.get()],
            |row| Cols(row).id(0),
        )
        .await?;
        if !live.is_empty() {
            let named: Vec<String> = live.iter().map(ToString::to_string).collect();
            return Err(StoreError::Invalid(format!(
                "{name} still has tasks that have not ended: {}. Cancel or finish them first",
                named.join(", ")
            )));
        }
        crate::deletion::preserve_floors(conn).await?;
        exec(
            conn,
            "DELETE FROM standing_rules WHERE agent_id = ?1",
            params![id.get()],
        )
        .await?;
        exec(
            conn,
            "DELETE FROM bindings WHERE agent_id = ?1",
            params![id.get()],
        )
        .await?;
        // The row stays for history; its name is freed for a new agent.
        let mut tombstone = format!("{name} (deleted)");
        let taken = query_opt(
            conn,
            "SELECT id FROM agents WHERE name = ?1",
            params![tombstone.as_str()],
            |row| Cols(row).i64(0),
        )
        .await?;
        if taken.is_some() {
            tombstone = format!("{name} (deleted {id})");
        }
        exec(
            conn,
            "UPDATE agents SET name = ?2, deleted_at = ?3 WHERE id = ?1",
            params![id.get(), tombstone.as_str(), self.now().as_millis()],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn bind(&self, binding: &NewBinding) -> StoreResult<Binding> {
        let tx = self.db.begin().await?;
        let id: i64 = query_one(
            tx.conn(),
            concat!(
                "INSERT INTO bindings (id, project_id, agent_id, mode, is_lead, max_parallel)
             VALUES (",
                crate::db::next_id_sql!("bindings"),
                ", ?1, ?2, ?3, ?4, ?5) RETURNING id"
            ),
            params![
                binding.project_id.get(),
                binding.agent_id.get(),
                binding.mode.as_str(),
                binding.is_lead,
                i64::from(binding.max_parallel),
            ],
            |row| Cols(row).i64(0),
        )
        .await?;
        let created = query_one(
            tx.conn(),
            &format!("SELECT {BINDING_COLUMNS} WHERE id = ?1"),
            params![id],
            decode_binding,
        )
        .await?;
        tx.commit().await?;
        Ok(created)
    }

    async fn set_binding_mode(&self, id: BindingId, mode: Mode) -> StoreResult<Binding> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "UPDATE bindings SET mode = ?2 WHERE id = ?1",
            params![id.get(), mode.as_str()],
        )
        .await?;
        let updated = query_opt(
            tx.conn(),
            &format!("SELECT {BINDING_COLUMNS} WHERE id = ?1"),
            params![id.get()],
            decode_binding,
        )
        .await?
        .ok_or_else(|| StoreError::not_found("binding", id))?;
        tx.commit().await?;
        Ok(updated)
    }

    async fn bindings(&self, project_id: ProjectId) -> StoreResult<Vec<Binding>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {BINDING_COLUMNS} WHERE project_id = ?1 ORDER BY id"),
            params![project_id.get()],
            decode_binding,
        )
        .await
    }

    async fn lead_binding(&self, project_id: ProjectId) -> StoreResult<Option<Binding>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {BINDING_COLUMNS} WHERE project_id = ?1 AND is_lead = 1"),
            params![project_id.get()],
            decode_binding,
        )
        .await
    }

    async fn set_project_ceiling(&self, id: ProjectId, ceiling: Option<Mode>) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let changed = exec(
            tx.conn(),
            "UPDATE projects SET mode_ceiling = ?2 WHERE id = ?1",
            params![id.get(), ceiling.map(Mode::as_str)],
        )
        .await?;
        if changed == 0 {
            return Err(StoreError::not_found("project", id));
        }
        tx.commit().await
    }

    async fn set_binding_overrides(&self, id: BindingId, overrides: &Json) -> StoreResult<Binding> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "UPDATE bindings SET overrides = ?2 WHERE id = ?1",
            params![id.get(), json_text(overrides)?],
        )
        .await?;
        let updated = query_opt(
            tx.conn(),
            &format!("SELECT {BINDING_COLUMNS} WHERE id = ?1"),
            params![id.get()],
            decode_binding,
        )
        .await?
        .ok_or_else(|| StoreError::not_found("binding", id))?;
        tx.commit().await?;
        Ok(updated)
    }

    async fn set_binding_max_parallel(
        &self,
        id: BindingId,
        max_parallel: u32,
    ) -> StoreResult<Binding> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "UPDATE bindings SET max_parallel = ?2 WHERE id = ?1",
            params![id.get(), i64::from(max_parallel)],
        )
        .await?;
        let updated = query_opt(
            tx.conn(),
            &format!("SELECT {BINDING_COLUMNS} WHERE id = ?1"),
            params![id.get()],
            decode_binding,
        )
        .await?
        .ok_or_else(|| StoreError::not_found("binding", id))?;
        tx.commit().await?;
        Ok(updated)
    }
}
