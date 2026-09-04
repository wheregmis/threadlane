use crate::system_prompt::SystemPromptConfig;
use serde::Serialize;
use std::path::PathBuf;
use threadlane_skills::SkillRegistry;
use threadlane_wasi::WasiExtensionManager;

#[derive(Clone)]
pub struct CodingAgentOptions {
    pub api_key: String,
    pub account_id: Option<String>,
    pub model: String,
    pub work_dir: PathBuf,
    pub session_file: Option<PathBuf>,
    pub system_prompt: SystemPromptConfig,
    /// Agent-level configuration (compaction, stream rules, etc.).
    pub agent_config: Option<threadlane_runtime::AgentConfig>,
    /// Coding-agent-specific configuration (subagents, WASI, etc.).
    pub coding_config: Option<crate::config::CodingAgentConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessCompositionSnapshot {
    pub active_lane: String,
    pub session_file: Option<String>,
    pub model: String,
    pub provider: String,
    pub skills: Vec<String>,
    pub extensions: Vec<String>,
    pub sandbox_policy: String,
}

impl HarnessCompositionSnapshot {
    fn from_options(options: &CodingAgentOptions) -> Self {
        let provider = if options.model.starts_with("antigravity/") {
            "antigravity"
        } else if options.model.starts_with("opencode-go/") {
            "opencode-go"
        } else if options.model.starts_with("acp/") {
            "acp"
        } else {
            "openai"
        };
        Self {
            active_lane: "main".into(),
            session_file: options
                .session_file
                .as_ref()
                .map(|path| path.display().to_string()),
            model: options.model.clone(),
            provider: provider.into(),
            skills: Vec::new(),
            extensions: Vec::new(),
            sandbox_policy: "workspace-scoped capabilities".into(),
        }
    }

    pub fn resolved(
        options: &CodingAgentOptions,
        skills: &SkillRegistry,
        extensions: &WasiExtensionManager,
    ) -> Self {
        let mut snapshot = Self::from_options(options);
        snapshot.skills = skills
            .list_skills()
            .into_iter()
            .filter(|skill| skill.enabled && skill.is_valid)
            .map(|skill| skill.id)
            .collect();
        snapshot.extensions = extensions
            .extension_manifests()
            .into_iter()
            .map(|extension| extension.name)
            .collect();
        snapshot
    }
}
