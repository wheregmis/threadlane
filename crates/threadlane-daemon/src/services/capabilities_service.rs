use std::path::Path;
use threadlane_protocol::capabilities::*;
use threadlane_protocol::project::default_global_threadlane_dir;
use threadlane_skills::{SkillManager, SkillSettings};

#[derive(Clone, Default)]
pub struct CapabilitiesService;

impl CapabilitiesService {
    pub fn new() -> Self {
        Self
    }

    pub fn list_models(&self) -> Result<ListModelsResponse, String> {
        Ok(ListModelsResponse {
            models: crate::model_catalog::models(),
        })
    }

    pub fn list_skills(&self, req: ListSkillsRequest) -> Result<ListSkillsResponse, String> {
        let project_dir = Path::new(&req.project_path);
        let mut manager = SkillManager::new();
        manager.discover_skills(Some(project_dir));
        let skills = manager
            .list_skills()
            .into_iter()
            .map(|s| SkillDescriptor {
                id: s.id,
                name: s.name,
                description: s.description,
                enabled: s.enabled,
                scope: format!("{:?}", s.scope).to_lowercase(),
            })
            .collect();

        Ok(ListSkillsResponse { skills })
    }

    pub fn toggle_skill(&self, req: ToggleSkillRequest) -> Result<(), String> {
        let project_dir = Path::new(&req.project_path);
        let mut settings = SkillSettings::load(project_dir);
        settings.set_enabled(project_dir, &req.skill_id, req.enabled)
    }

    pub fn get_daemon_info(&self) -> Result<DaemonInfoResponse, String> {
        Ok(DaemonInfoResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: threadlane_protocol::JSONRPC_VERSION.to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            workspace_count: 1,
        })
    }

    // ── ACP Agent CRUD ────────────────────────────────────────────────────

    fn acp_path(project_path: Option<&Path>, scope: AcpScope) -> Result<std::path::PathBuf, String> {
        match scope {
            AcpScope::Global => default_global_threadlane_dir()
                .map(|d| d.join("acp.json"))
                .ok_or_else(|| "Global Threadlane dir unavailable".to_string()),
            AcpScope::Project => project_path
                .map(|p| p.join(".threadlane").join("acp.json"))
                .ok_or_else(|| "Project root required for project-scoped ACP".to_string()),
        }
    }

    fn load_acp(path: &std::path::Path) -> Vec<AcpAgentConfig> {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_acp(path: &std::path::Path, agents: &[AcpAgentConfig]) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let data = serde_json::to_vec_pretty(agents).map_err(|e| e.to_string())?;
        std::fs::write(path, data).map_err(|e| e.to_string())
    }

    pub fn list_acp_agents(&self, req: ListAcpAgentsRequest) -> Result<ListAcpAgentsResponse, String> {
        let project = req.project_path.as_deref().map(Path::new);
        let mut agents = vec![];

        // Global scope
        if let Ok(path) = Self::acp_path(project, AcpScope::Global) {
            for config in Self::load_acp(&path) {
                agents.push(AcpAgentRecord {
                    status: if config.enabled {
                        AcpAgentStatus::Connected
                    } else {
                        AcpAgentStatus::Disconnected
                    },
                    config,
                });
            }
        }

        // Project scope (if project_path provided)
        if project.is_some() {
            if let Ok(path) = Self::acp_path(project, AcpScope::Project) {
                for config in Self::load_acp(&path) {
                    agents.push(AcpAgentRecord {
                        status: if config.enabled {
                            AcpAgentStatus::Connected
                        } else {
                            AcpAgentStatus::Disconnected
                        },
                        config,
                    });
                }
            }
        }

        Ok(ListAcpAgentsResponse { agents })
    }

    pub fn add_acp_agent(&self, req: AddAcpAgentRequest) -> Result<(), String> {
        if req.command.trim().starts_with("http://") || req.command.trim().starts_with("https://") {
            return Err("ACP agents must be local stdio commands, not URLs.".into());
        }
        let config = AcpAgentConfig::from_command_line(&req.name, &req.command, req.scope)
            .ok_or_else(|| "Enter both an agent name and command.".to_string())?;
        let project = req.project_path.as_deref().map(Path::new);
        let path = Self::acp_path(project, req.scope)?;
        let mut agents = Self::load_acp(&path);
        agents.retain(|a| a.id != config.id);
        agents.push(config);
        Self::save_acp(&path, &agents)
    }

    pub fn set_acp_enabled(&self, req: SetAcpEnabledRequest) -> Result<(), String> {
        let project = req.project_path.as_deref().map(Path::new);
        let path = Self::acp_path(project, req.scope)?;
        let mut agents = Self::load_acp(&path);
        let agent = agents
            .iter_mut()
            .find(|a| a.id == req.id)
            .ok_or_else(|| "ACP agent not found. Please refresh.".to_string())?;
        agent.enabled = req.enabled;
        Self::save_acp(&path, &agents)
    }

    pub fn remove_acp_agent(&self, req: RemoveAcpAgentRequest) -> Result<(), String> {
        let project = req.project_path.as_deref().map(Path::new);
        let path = Self::acp_path(project, req.scope)?;
        let mut agents = Self::load_acp(&path);
        let prev_len = agents.len();
        agents.retain(|a| a.id != req.id);
        if agents.len() == prev_len {
            return Err("ACP agent not found. Please refresh.".into());
        }
        Self::save_acp(&path, &agents)
    }

    /// Normalize title strings from the session using the shared protocol helper.
    pub fn generate_title(
        &self,
        req: GenerateTitleRequest,
    ) -> Result<GenerateTitleResponse, String> {
        // Lightweight stub: use the submitted prompt as title seed, normalized.
        // A full implementation would call the model provider.
        let title = threadlane_protocol::normalize_session_title(&req.prompt);
        Ok(GenerateTitleResponse { title })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_models_uses_the_daemon_catalog() {
        assert_eq!(
            CapabilitiesService::new().list_models().unwrap().models,
            crate::model_catalog::models(),
        );
    }
}
