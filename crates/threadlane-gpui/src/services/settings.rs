//! Settings service client — all file I/O delegated to the daemon.

use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_protocol::{
    AcpAgentRecord, AcpAgentStatus, AcpScope, ExtensionRecord, ExtensionScope, SkillMetadata,
    SkillScope, AddAcpAgentRequest, RemoveAcpAgentRequest, SetAcpEnabledRequest,
    GetSubagentSettingsRequest,
};

pub use threadlane_protocol::{
    AcpAgentConfig, AcpAgentRecord as AcpAgentRecordAlias, AcpAgentStatus as AcpAgentStatusAlias,
    AcpScope as AcpScopeAlias, ExtensionRecord as ExtensionRecordAlias,
    ExtensionScope as ExtensionScopeAlias, SkillMetadata as SkillMetadataAlias,
};

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    crate::services::chat::executor()
}

#[derive(Debug)]
pub enum SettingsEvent {
    AcpRefreshed(Vec<AcpAgentRecord>),
}

// ── Needle ────────────────────────────────────────────────────────────────────

pub(crate) fn load_needle_enabled() -> bool {
    if let Ok(rt) = executor() {
        rt.block_on(async {
            let client = crate::services::daemon_client::get_daemon_client().await?;
            client.get_needle_enabled().await.map(|r| r.enabled)
        })
        .unwrap_or(false)
    } else {
        false
    }
}

pub(crate) fn save_needle_enabled(enabled: bool) -> Result<(), String> {
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .set_needle_enabled(threadlane_protocol::SetNeedleEnabledRequest { enabled })
            .await
    })
}

// ── Extension stubs ───────────────────────────────────────────────────────────
// Extension management is handled by the daemon; these stubs satisfy the GPUI
// UI call-sites until they are wired to their respective RPC methods.

pub(crate) fn discover_extensions(_project_root: Option<std::path::PathBuf>) -> Vec<ExtensionRecord> {
    Vec::new()
}

pub(crate) fn install_extension(
    _project_root: Option<std::path::PathBuf>,
    _source: &std::path::Path,
    _scope: ExtensionScope,
) -> Result<String, String> {
    Ok("Extension installation managed by daemon".into())
}

pub(crate) fn set_extension_enabled(
    _project_root: Option<std::path::PathBuf>,
    _target: &ExtensionRecord,
    _enabled: bool,
) -> Result<(), String> {
    Ok(())
}

pub(crate) fn remove_extension(
    _project_root: Option<std::path::PathBuf>,
    _target: &ExtensionRecord,
) -> Result<(), String> {
    Ok(())
}

// ── Skills ────────────────────────────────────────────────────────────────────

pub(crate) fn discover_skills(project_root: Option<&std::path::Path>) -> Vec<SkillMetadata> {
    // Skills are enumerated by the daemon via capabilities/skills.
    // Return empty; the GPUI settings panel should call the daemon async.
    Vec::new()
}

pub(crate) fn set_skill_enabled(
    project_root: &std::path::Path,
    skill_id: &str,
    enabled: bool,
) -> Result<(), String> {
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .toggle_skill(threadlane_protocol::ToggleSkillRequest {
                project_path: project_root.to_string_lossy().to_string(),
                skill_id: skill_id.to_string(),
                enabled,
            })
            .await
    })
}

pub(crate) fn disable_all_skills(
    project_root: &std::path::Path,
    skill_ids: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    let rt = executor()?;
    for id in skill_ids {
        rt.block_on(async {
            let client = crate::services::daemon_client::get_daemon_client().await?;
            client
                .toggle_skill(threadlane_protocol::ToggleSkillRequest {
                    project_path: project_root.to_string_lossy().to_string(),
                    skill_id: id,
                    enabled: false,
                })
                .await
        })?;
    }
    Ok(())
}

// ── ACP Agents ────────────────────────────────────────────────────────────────

pub(crate) fn configured_acp_agents(project_root: Option<std::path::PathBuf>) -> Vec<AcpAgentRecord> {
    executor()
        .ok()
        .and_then(|rt| {
            rt.block_on(async {
                let client = crate::services::daemon_client::get_daemon_client().await?;
                client
                    .list_acp_agents(threadlane_protocol::ListAcpAgentsRequest {
                        project_path: project_root
                            .map(|p| p.to_string_lossy().to_string()),
                    })
                    .await
                    .map(|r| r.agents)
            })
            .ok()
        })
        .unwrap_or_default()
}

pub(crate) fn probe_acp_agents(
    project_root: Option<std::path::PathBuf>,
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
        self.previous_commands.contains(&agent.command_line().as_str())
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

pub(crate) fn upgrade_acp_presets(project_root: Option<&std::path::Path>) -> Result<(), String> {
    let rt = executor()?;
    let scopes: &[AcpScope] = if project_root.is_some() {
        &[AcpScope::Global, AcpScope::Project]
    } else {
        &[AcpScope::Global]
    };
    for &scope in scopes {
        let agents = rt.block_on(async {
            let client = crate::services::daemon_client::get_daemon_client().await?;
            client
                .list_acp_agents(threadlane_protocol::ListAcpAgentsRequest {
                    project_path: project_root.map(|p| p.to_string_lossy().to_string()),
                })
                .await
                .map(|r| r.agents)
        })?;
        for preset in ACP_PRESETS {
            if let Some(record) = agents.iter().find(|a| preset.matches_agent(&a.config)) {
                if preset.needs_command_upgrade(&record.config) {
                    let mut new_config = preset.to_agent_config(scope);
                    new_config.enabled = record.config.enabled;
                    rt.block_on(async {
                        let client =
                            crate::services::daemon_client::get_daemon_client().await?;
                        // Remove old entry and add upgraded one.
                        client
                            .remove_acp_agent(RemoveAcpAgentRequest {
                                id: record.config.id.clone(),
                                scope,
                                project_path: project_root
                                    .map(|p| p.to_string_lossy().to_string()),
                            })
                            .await?;
                        client
                            .add_acp_agent(AddAcpAgentRequest {
                                name: new_config.name.clone(),
                                command: new_config.command_line(),
                                scope,
                                project_path: project_root
                                    .map(|p| p.to_string_lossy().to_string()),
                            })
                            .await
                    })?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn set_acp_preset_enabled(
    project_root: Option<&std::path::Path>,
    scope: AcpScope,
    preset: &AcpPreset,
    enabled: bool,
) -> Result<(), String> {
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        // Ensure the preset agent exists, then toggle.
        let agents = client
            .list_acp_agents(threadlane_protocol::ListAcpAgentsRequest {
                project_path: project_root.map(|p| p.to_string_lossy().to_string()),
            })
            .await?
            .agents;
        if !agents.iter().any(|a| preset.matches_agent(&a.config)) {
            let mut config = preset.to_agent_config(scope);
            config.enabled = enabled;
            client
                .add_acp_agent(AddAcpAgentRequest {
                    name: config.name.clone(),
                    command: config.command_line(),
                    scope,
                    project_path: project_root.map(|p| p.to_string_lossy().to_string()),
                })
                .await?;
        } else {
            client
                .set_acp_enabled(SetAcpEnabledRequest {
                    id: preset.id.to_string(),
                    enabled,
                    scope,
                    project_path: project_root.map(|p| p.to_string_lossy().to_string()),
                })
                .await?;
        }
        Ok(())
    })
}

pub(crate) fn add_acp_agent(
    project_root: Option<&std::path::Path>,
    scope: AcpScope,
    name: &str,
    command: &str,
) -> Result<(), String> {
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .add_acp_agent(AddAcpAgentRequest {
                name: name.to_string(),
                command: command.to_string(),
                scope,
                project_path: project_root.map(|p| p.to_string_lossy().to_string()),
            })
            .await
    })
}

pub(crate) fn set_acp_enabled(
    project_root: Option<&std::path::Path>,
    scope: AcpScope,
    id: &str,
    enabled: bool,
) -> Result<(), String> {
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .set_acp_enabled(SetAcpEnabledRequest {
                id: id.to_string(),
                enabled,
                scope,
                project_path: project_root.map(|p| p.to_string_lossy().to_string()),
            })
            .await
    })
}

pub(crate) fn remove_acp_agent(
    project_root: Option<&std::path::Path>,
    scope: AcpScope,
    id: &str,
) -> Result<(), String> {
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .remove_acp_agent(RemoveAcpAgentRequest {
                id: id.to_string(),
                scope,
                project_path: project_root.map(|p| p.to_string_lossy().to_string()),
            })
            .await
    })
}
