use std::path::Path;
use threadlane_protocol::capabilities::*;
use threadlane_skills::{SkillManager, SkillSettings};

#[derive(Clone, Default)]
pub struct CapabilitiesService;

impl CapabilitiesService {
    pub fn new() -> Self {
        Self
    }

    pub fn list_models(&self) -> Result<ListModelsResponse, String> {
        let models = vec![
            ModelDescriptor {
                id: "antigravity/gemini-3.7-flash".to_string(),
                name: "Gemini 3.7 Flash".to_string(),
                provider: "antigravity".to_string(),
                supports_reasoning: true,
                context_window: Some(1_000_000),
            },
            ModelDescriptor {
                id: "antigravity/gemini-3.7-pro".to_string(),
                name: "Gemini 3.7 Pro".to_string(),
                provider: "antigravity".to_string(),
                supports_reasoning: true,
                context_window: Some(2_000_000),
            },
            ModelDescriptor {
                id: "gpt-4o".to_string(),
                name: "GPT-4o".to_string(),
                provider: "openai".to_string(),
                supports_reasoning: false,
                context_window: Some(128_000),
            },
            ModelDescriptor {
                id: "o3-mini".to_string(),
                name: "o3-mini".to_string(),
                provider: "openai".to_string(),
                supports_reasoning: true,
                context_window: Some(200_000),
            },
        ];

        Ok(ListModelsResponse { models })
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
}
