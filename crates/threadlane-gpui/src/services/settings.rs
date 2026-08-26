use std::path::{Path, PathBuf};
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_session::{
    AcpAgentConfig, AcpAgentRecord, AcpAgentStatus, AcpManager, AcpScope, AcpSettings,
    ExtensionManager, ExtensionRecord, ExtensionScope, SkillManager, SkillMetadata, SkillSettings,
    default_global_threadlane_dir,
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
    if enabled {
        threadlane_runtime::local_tool_router::validate_needle_model()?;
    }
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
    Ok(threadlane_runtime::get_runtime())
}

fn extension_manager(project_root: Option<PathBuf>) -> ExtensionManager {
    ExtensionManager::new(default_global_threadlane_dir(), project_root)
}

pub(crate) fn discover_extensions(project_root: Option<PathBuf>) -> Vec<ExtensionRecord> {
    extension_manager(project_root).discover()
}

pub(crate) fn install_extension(
    project_root: Option<PathBuf>,
    source: &Path,
    scope: ExtensionScope,
) -> Result<String, String> {
    let record = extension_manager(project_root).install_from_wasm(source, scope)?;
    Ok(format!(
        "Installed {} v{}.",
        record.name(),
        record.version()
    ))
}

pub(crate) fn set_extension_enabled(
    project_root: Option<PathBuf>,
    target: &ExtensionRecord,
    enabled: bool,
) -> Result<(), String> {
    let manager = extension_manager(project_root);
    let current = manager
        .discover()
        .into_iter()
        .find(|record| {
            record.id() == target.id()
                && record.scope() == target.scope()
                && record.module_path() == target.module_path()
        })
        .ok_or_else(|| "Extension inventory changed. Please try again.".to_string())?;
    manager.set_enabled(&current, enabled)
}

pub(crate) fn remove_extension(
    project_root: Option<PathBuf>,
    target: &ExtensionRecord,
) -> Result<(), String> {
    let manager = extension_manager(project_root);
    let current = manager
        .discover()
        .into_iter()
        .find(|record| {
            record.id() == target.id()
                && record.scope() == target.scope()
                && record.module_path() == target.module_path()
        })
        .ok_or_else(|| "Extension inventory changed. Please try again.".to_string())?;
    manager.remove(&current)
}

pub(crate) fn discover_skills(project_root: Option<&Path>) -> Vec<SkillMetadata> {
    let mut manager = SkillManager::new();
    manager.discover_skills(project_root);
    manager.list_skills()
}

pub(crate) fn set_skill_enabled(
    project_root: &Path,
    skill_id: &str,
    enabled: bool,
) -> Result<(), String> {
    SkillSettings::load(project_root).set_enabled(project_root, skill_id, enabled)
}

pub(crate) fn disable_all_skills(
    project_root: &Path,
    skill_ids: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    let mut settings = SkillSettings::load(project_root);
    settings.disable_all(project_root, skill_ids)
}

fn acp_manager(project_root: Option<PathBuf>) -> AcpManager {
    AcpManager::new(default_global_threadlane_dir(), project_root)
}

pub(crate) fn configured_acp_agents(project_root: Option<PathBuf>) -> Vec<AcpAgentRecord> {
    acp_manager(project_root)
        .configs()
        .into_iter()
        .map(|config| AcpAgentRecord {
            status: if config.enabled {
                AcpAgentStatus::Connecting
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
    executor()?.spawn(async move {
        let records = acp_manager(project_root).discover_and_connect().await;
        let _ = tx.send(SettingsEvent::AcpRefreshed(records));
    });
    Ok(())
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
    match scope {
        AcpScope::Global => Ok(AcpSettings::load_global(
            default_global_threadlane_dir().as_deref(),
        )),
        AcpScope::Project => {
            let root = project_root.ok_or_else(|| "Attach a project first.".to_string())?;
            Ok(AcpSettings::load_project(Some(root)))
        }
    }
}

fn save_acp_scope(
    project_root: Option<&Path>,
    scope: AcpScope,
    agents: &[AcpAgentConfig],
) -> Result<(), String> {
    match scope {
        AcpScope::Global => {
            let root = default_global_threadlane_dir()
                .ok_or_else(|| "Global Threadlane directory is unavailable.".to_string())?;
            AcpSettings::save_global(&root, agents)
        }
        AcpScope::Project => {
            let root = project_root.ok_or_else(|| "Attach a project first.".to_string())?;
            AcpSettings::save_project(root, agents)
        }
    }
}
