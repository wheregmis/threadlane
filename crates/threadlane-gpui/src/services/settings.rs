use std::path::{Path, PathBuf};
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_protocol::project::default_global_threadlane_dir;
pub use threadlane_protocol::{
    AcpAgentConfig, AcpAgentRecord, AcpAgentStatus, AcpScope, ExtensionRecord, ExtensionScope,
    SkillMetadata,
};

fn needle_preferences_path() -> Option<PathBuf> {
    default_global_threadlane_dir().map(|dir| dir.join("gui").join("needle.json"))
}

pub(crate) fn load_needle_enabled() -> bool {
    needle_preferences_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<bool>(&bytes).ok())
        .unwrap_or(false)
}

pub(crate) fn save_needle_enabled(enabled: bool) -> Result<(), String> {
    let path = needle_preferences_path()
        .ok_or_else(|| "Global settings directory is unavailable.".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "Needle settings path has no parent.".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(&enabled).map_err(|error| error.to_string())?;
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}

#[derive(Debug)]
pub enum SettingsEvent {
    AcpRefreshed(Vec<AcpAgentRecord>),
}

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    crate::services::chat::executor()
}

pub(crate) fn discover_extensions(_project_root: Option<PathBuf>) -> Vec<ExtensionRecord> {
    Vec::new()
}

pub(crate) fn install_extension(
    _project_root: Option<PathBuf>,
    _source: &Path,
    _scope: ExtensionScope,
) -> Result<String, String> {
    Ok("Extension installation managed by daemon".into())
}

pub(crate) fn set_extension_enabled(
    _project_root: Option<PathBuf>,
    _target: &ExtensionRecord,
    _enabled: bool,
) -> Result<(), String> {
    Ok(())
}

pub(crate) fn remove_extension(
    _project_root: Option<PathBuf>,
    _target: &ExtensionRecord,
) -> Result<(), String> {
    Ok(())
}

pub(crate) fn discover_skills(project_root: Option<&Path>) -> Vec<SkillMetadata> {
    let skills_file = match project_root {
        Some(p) => p.join(".threadlane").join("skills.json"),
        None => match default_global_threadlane_dir() {
            Some(g) => g.join("skills.json"),
            None => return Vec::new(),
        },
    };
    if let Ok(bytes) = std::fs::read(&skills_file) {
        if let Ok(skills) = serde_json::from_slice::<Vec<SkillMetadata>>(&bytes) {
            return skills;
        }
    }
    Vec::new()
}

pub(crate) fn set_skill_enabled(
    project_root: &Path,
    skill_id: &str,
    enabled: bool,
) -> Result<(), String> {
    let skills_file = project_root.join(".threadlane").join("skills.json");
    let mut skills = discover_skills(Some(project_root));
    if let Some(skill) = skills.iter_mut().find(|s| s.id == skill_id) {
        skill.enabled = enabled;
    }
    let data = serde_json::to_vec_pretty(&skills).map_err(|e| e.to_string())?;
    std::fs::write(&skills_file, data).map_err(|e| e.to_string())
}

pub(crate) fn disable_all_skills(
    project_root: &Path,
    skill_ids: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    let ids: std::collections::HashSet<String> = skill_ids.into_iter().collect();
    let skills_file = project_root.join(".threadlane").join("skills.json");
    let mut skills = discover_skills(Some(project_root));
    for skill in skills.iter_mut() {
        if ids.contains(&skill.id) {
            skill.enabled = false;
        }
    }
    let data = serde_json::to_vec_pretty(&skills).map_err(|e| e.to_string())?;
    std::fs::write(&skills_file, data).map_err(|e| e.to_string())
}

pub(crate) fn configured_acp_agents(project_root: Option<PathBuf>) -> Vec<AcpAgentRecord> {
    load_acp_scope(project_root.as_deref(), AcpScope::Global)
        .unwrap_or_default()
        .into_iter()
        .map(|config| AcpAgentRecord {
            status: if config.enabled {
                AcpAgentStatus::Connected
            } else {
                AcpAgentStatus::Disconnected
            },
            config,
        })
        .collect()
}

pub(crate) fn probe_acp_agents(
    project_root: Option<PathBuf>,
    tx: Sender<SettingsEvent>,
) -> Result<(), String> {
    let records = configured_acp_agents(project_root);
    let _ = tx.send(SettingsEvent::AcpRefreshed(records));
    Ok(())
}

pub(crate) struct AcpPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub command: &'static str,
    previous_commands: &'static [&'static str],
}

impl AcpPreset {
    pub fn matches_agent(&self, agent: &AcpAgentConfig) -> bool {
        self.id == agent.id
    }

    pub fn needs_command_upgrade(&self, agent: &AcpAgentConfig) -> bool {
        self.previous_commands
            .contains(&agent.command_line().as_str())
    }

    pub fn to_agent_config(&self, scope: AcpScope) -> AcpAgentConfig {
        AcpAgentConfig::from_command_line(self.name, self.command, scope)
            .expect("built-in ACP presets must have a name and command")
    }
}

pub(crate) const ACP_PRESETS: &[AcpPreset] = &[
    AcpPreset {
        id: "claude_code",
        name: "Claude Code",
        description: "Use Anthropic's Claude Code agent through ACP.",
        command: "npx -y @zed-industries/claude-code-acp",
        previous_commands: &[],
    },
    AcpPreset {
        id: "codex",
        name: "Codex",
        description: "Use OpenAI Codex through ACP.",
        command: "npx -y @agentclientprotocol/codex-acp",
        previous_commands: &["npx -y @zed-industries/codex-acp"],
    },
];

pub(crate) fn upgrade_acp_presets(project_root: Option<&Path>) -> Result<(), String> {
    let scopes = if project_root.is_some() {
        &[AcpScope::Global, AcpScope::Project][..]
    } else {
        &[AcpScope::Global][..]
    };
    for &scope in scopes {
        let mut agents = load_acp_scope(project_root, scope)?;
        let mut changed = false;
        for preset in ACP_PRESETS {
            let Some(agent) = agents.iter_mut().find(|agent| preset.matches_agent(agent)) else {
                continue;
            };
            if preset.needs_command_upgrade(agent) {
                let enabled = agent.enabled;
                *agent = preset.to_agent_config(scope);
                agent.enabled = enabled;
                changed = true;
            }
        }
        if changed {
            save_acp_scope(project_root, scope, &agents)?;
        }
    }
    Ok(())
}

pub(crate) fn set_acp_preset_enabled(
    project_root: Option<&Path>,
    scope: AcpScope,
    preset: &AcpPreset,
    enabled: bool,
) -> Result<(), String> {
    let mut agents = load_acp_scope(project_root, scope)?;
    if let Some(agent) = agents.iter_mut().find(|agent| preset.matches_agent(agent)) {
        if preset.needs_command_upgrade(agent) {
            *agent = preset.to_agent_config(scope);
        }
        agent.enabled = enabled;
    } else {
        let mut config = preset.to_agent_config(scope);
        config.enabled = enabled;
        agents.push(config);
    }
    save_acp_scope(project_root, scope, &agents)
}

pub(crate) fn add_acp_agent(
    project_root: Option<&Path>,
    scope: AcpScope,
    name: &str,
    command: &str,
) -> Result<(), String> {
    if command.trim().starts_with("http://") || command.trim().starts_with("https://") {
        return Err("ACP agents must be local stdio commands, not URLs.".into());
    }
    let config = AcpAgentConfig::from_command_line(name, command, scope)
        .ok_or_else(|| "Enter both an agent name and command.".to_string())?;
    let mut agents = load_acp_scope(project_root, scope)?;
    agents.retain(|agent| agent.id != config.id);
    agents.push(config);
    save_acp_scope(project_root, scope, &agents)
}

pub(crate) fn set_acp_enabled(
    project_root: Option<&Path>,
    scope: AcpScope,
    id: &str,
    enabled: bool,
) -> Result<(), String> {
    let mut agents = load_acp_scope(project_root, scope)?;
    let agent = agents
        .iter_mut()
        .find(|agent| agent.id == id)
        .ok_or_else(|| "ACP agent list changed. Please refresh.".to_string())?;
    agent.enabled = enabled;
    save_acp_scope(project_root, scope, &agents)
}

pub(crate) fn remove_acp_agent(
    project_root: Option<&Path>,
    scope: AcpScope,
    id: &str,
) -> Result<(), String> {
    let mut agents = load_acp_scope(project_root, scope)?;
    let previous_len = agents.len();
    agents.retain(|agent| agent.id != id);
    if agents.len() == previous_len {
        return Err("ACP agent list changed. Please refresh.".into());
    }
    save_acp_scope(project_root, scope, &agents)
}

fn load_acp_scope(
    project_root: Option<&Path>,
    scope: AcpScope,
) -> Result<Vec<AcpAgentConfig>, String> {
    let target = match scope {
        AcpScope::Global => default_global_threadlane_dir()
            .map(|d| d.join("acp.json"))
            .ok_or_else(|| "Global Threadlane dir unavailable".to_string())?,
        AcpScope::Project => project_root
            .map(|p| p.join(".threadlane").join("acp.json"))
            .ok_or_else(|| "Project root unavailable".to_string())?,
    };
    if let Ok(bytes) = std::fs::read(&target) {
        if let Ok(configs) = serde_json::from_slice::<Vec<AcpAgentConfig>>(&bytes) {
            return Ok(configs);
        }
    }
    Ok(Vec::new())
}

fn save_acp_scope(
    project_root: Option<&Path>,
    scope: AcpScope,
    agents: &[AcpAgentConfig],
) -> Result<(), String> {
    let target = match scope {
        AcpScope::Global => default_global_threadlane_dir()
            .map(|d| d.join("acp.json"))
            .ok_or_else(|| "Global Threadlane dir unavailable".to_string())?,
        AcpScope::Project => project_root
            .map(|p| p.join(".threadlane").join("acp.json"))
            .ok_or_else(|| "Project root unavailable".to_string())?,
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_vec_pretty(agents).map_err(|e| e.to_string())?;
    std::fs::write(&target, data).map_err(|e| e.to_string())
}
