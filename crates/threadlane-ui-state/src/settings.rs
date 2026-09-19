use std::path::PathBuf;
use tokio::sync::mpsc::UnboundedSender as Sender;

use threadlane_project::default_global_threadlane_dir;
use threadlane_acp::{AcpAgentConfig, AcpAgentRecord, AcpManager};

#[derive(Debug)]
pub enum SettingsEvent {
    AcpRefreshed { request_id: u64, records: Vec<AcpAgentRecord> },
}

/// Retained by the settings screen for the lifetime of the workspace window.
#[derive(Default)]
pub struct AcpProbeState {
    context: Option<(Option<PathBuf>, Vec<AcpAgentConfig>)>,
    request_id: u64,
}

impl AcpProbeState {
    pub fn begin(
        &mut self,
        project: Option<PathBuf>,
        configs: Vec<AcpAgentConfig>,
        force: bool,
    ) -> Option<u64> {
        let context = (project, configs);
        if !force && self.context.as_ref() == Some(&context) {
            return None;
        }
        self.context = Some(context);
        self.request_id = self.request_id.wrapping_add(1);
        Some(self.request_id)
    }

    pub fn is_current(&self, request_id: u64) -> bool {
        self.context.is_some() && self.request_id == request_id
    }
}

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    Ok(threadlane_provider::exec::get_runtime())
}

fn acp_manager(project_root: Option<PathBuf>) -> AcpManager {
    AcpManager::new(default_global_threadlane_dir(), project_root)
}

pub fn probe_acp_agents(
    project_root: Option<PathBuf>,
    request_id: u64,
    tx: Sender<SettingsEvent>,
) -> Result<(), String> {
    executor()?.spawn(async move {
        let records = acp_manager(project_root).discover_and_connect().await;
        let _ = tx.send(SettingsEvent::AcpRefreshed { request_id, records });
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_acp::AcpScope;

    #[test]
    fn acp_tab_visits_reuse_probe_until_configuration_or_refresh_changes() {
        let mut state = AcpProbeState::default();
        let mut configs = vec![AcpAgentConfig::from_command_line(
            "Test agent", "test-agent --acp", AcpScope::Global,
        ).unwrap()];
        let first = state.begin(None, configs.clone(), false).unwrap();
        // Both an in-flight visit and a later visit reuse the same result.
        assert_eq!(state.begin(None, configs.clone(), false), None);
        assert!(state.is_current(first));
        let refresh = state.begin(None, configs.clone(), true).unwrap();
        assert!(!state.is_current(first));
        assert!(state.is_current(refresh));
        configs[0].enabled = !configs[0].enabled;
        let changed = state.begin(None, configs.clone(), false).unwrap();
        assert!(!state.is_current(refresh));
        assert!(state.is_current(changed));
        let project = Some(PathBuf::from("/another-project"));
        let switched = state.begin(project.clone(), configs.clone(), false).unwrap();
        assert!(!state.is_current(changed));
        assert!(state.is_current(switched));
        assert_eq!(state.begin(project, configs, false), None);
        // Empty configuration is also cached, rather than mistaken for uninitialized.
        assert!(state.begin(None, Vec::new(), false).is_some());
        assert_eq!(state.begin(None, Vec::new(), false), None);
    }
}
