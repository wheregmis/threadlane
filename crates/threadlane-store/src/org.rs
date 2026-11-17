//! [`OrgStore`] on the hub database.

use async_trait::async_trait;
use threadlane_core::ids::{AgentId, BindingId, LaneId, ProjectId};
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
     p.max_parallel_tasks, p.paused, l.id
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
    })
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
        "INSERT INTO projects (name, description, is_general, mode_ceiling, max_parallel_tasks, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
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
        "INSERT INTO lanes (project_id, kind) VALUES (?1, ?2) RETURNING id",
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

    async fn create_repository(&self, repository: &NewRepository) -> StoreResult<Repository> {
        let tx = self.db.begin().await?;
        let created = query_one(
            tx.conn(),
            &format!(
                "INSERT INTO repositories (project_id, name, remote_url, default_branch, protected_branches)
                 VALUES (?1, ?2, ?3, ?4, ?5) RETURNING {REPOSITORY_FIELDS}"
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
            &format!("SELECT {AGENT_COLUMNS} ORDER BY id"),
            (),
            decode_agent,
        )
        .await
    }

    async fn bind(&self, binding: &NewBinding) -> StoreResult<Binding> {
        let tx = self.db.begin().await?;
        let id: i64 = query_one(
            tx.conn(),
            "INSERT INTO bindings (project_id, agent_id, mode, is_lead, max_parallel)
             VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
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
}
