use crate::{default_global_threadlane_dir, AcpSettings};
use crate::{AcpAgentConfig, AcpScope};
use std::path::Path;

pub fn configured_acp_agents(
    project_root: Option<std::path::PathBuf>,
) -> Vec<crate::AcpAgentRecord> {
    crate::AcpManager::new(default_global_threadlane_dir(), project_root)
        .configs()
        .into_iter()
        .map(|config| crate::AcpAgentRecord {
            status: if config.enabled {
                crate::AcpAgentStatus::Connecting
            } else {
                crate::AcpAgentStatus::Disconnected
            },
            config,
        })
        .collect()
}

pub struct AcpPreset {
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

pub const ACP_PRESETS: &[AcpPreset] = &[
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
    AcpPreset {
        id: "opencode2",
        name: "OpenCode (ACP)",
        description: "Use OpenCode as an external ACP agent.",
        command: "opencode acp",
        previous_commands: &["opencode2 acp"],
    },
    AcpPreset {
        id: "antigravity",
        name: "Google Antigravity",
        description: "Use Google's Antigravity agent through ACP.",
        command: "agy_acp_server.par",
        previous_commands: &["./agy_acp_server.par"],
    },
];

pub fn upgrade_acp_presets(project_root: Option<&Path>) -> Result<(), String> {
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

pub fn set_acp_preset_enabled(
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

pub fn add_acp_agent(
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

pub fn set_acp_enabled(
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

pub fn remove_acp_agent(
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
    match scope {
        AcpScope::Global => Ok(AcpSettings::load_global(
            default_global_threadlane_dir().as_deref(),
        )),
        AcpScope::Project => Ok(AcpSettings::load_project(Some(
            project_root.ok_or_else(|| "Attach a project first.".to_string())?,
        ))),
    }
}

fn save_acp_scope(
    project_root: Option<&Path>,
    scope: AcpScope,
    agents: &[AcpAgentConfig],
) -> Result<(), String> {
    match scope {
        AcpScope::Global => AcpSettings::save_global(
            &default_global_threadlane_dir()
                .ok_or_else(|| "Global Threadlane directory is unavailable.".to_string())?,
            agents,
        ),
        AcpScope::Project => AcpSettings::save_project(
            project_root.ok_or_else(|| "Attach a project first.".to_string())?,
            agents,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_remote_acp_commands() {
        let error =
            add_acp_agent(None, AcpScope::Global, "remote", "https://example.test").unwrap_err();
        assert!(error.contains("local stdio"));
    }
}
