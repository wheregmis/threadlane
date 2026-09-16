use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_project::default_global_threadlane_dir;
pub(crate) use threadlane_session::settings::{
    disable_all_skills, discover_extensions, discover_skills, install_extension, remove_extension,
    set_extension_enabled, set_skill_enabled,
};
pub(crate) use threadlane_session::ACP_PRESETS;
pub(crate) use threadlane_session::{
    add_acp_agent, configured_acp_agents, remove_acp_agent, set_acp_enabled,
    set_acp_preset_enabled, upgrade_acp_presets,
};
use threadlane_session::{AcpAgentRecord, AcpManager};

#[derive(Debug)]
pub enum SettingsEvent {
    AcpRefreshed(Vec<AcpAgentRecord>),
}

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    Ok(threadlane_runtime::get_runtime())
}

fn acp_manager(project_root: Option<PathBuf>) -> AcpManager {
    AcpManager::new(default_global_threadlane_dir(), project_root)
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
