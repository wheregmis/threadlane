use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_project::default_global_threadlane_dir;
use threadlane_acp::{AcpAgentRecord, AcpManager};

#[derive(Debug)]
pub enum SettingsEvent {
    AcpRefreshed(Vec<AcpAgentRecord>),
}

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    Ok(threadlane_provider::exec::get_runtime())
}

fn acp_manager(project_root: Option<PathBuf>) -> AcpManager {
    AcpManager::new(default_global_threadlane_dir(), project_root)
}

pub fn probe_acp_agents(
    project_root: Option<PathBuf>,
    tx: Sender<SettingsEvent>,
) -> Result<(), String> {
    executor()?.spawn(async move {
        let records = acp_manager(project_root).discover_and_connect().await;
        let _ = tx.send(SettingsEvent::AcpRefreshed(records));
    });
    Ok(())
}
