//! Subagent settings client — delegates file I/O to the daemon.

use std::path::Path;
use threadlane_protocol::{
    GetSubagentSettingsRequest, ReasoningEffort, SetSubagentSettingsRequest, SubagentSettingsData,
};

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    crate::services::chat::executor()
}

// Re-export SubagentSettings as a GPUI-local alias backed by the protocol type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SubagentSettings {
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

impl From<SubagentSettingsData> for SubagentSettings {
    fn from(d: SubagentSettingsData) -> Self {
        Self {
            model: d.model,
            reasoning_effort: d.reasoning_effort,
        }
    }
}

impl From<SubagentSettings> for SubagentSettingsData {
    fn from(s: SubagentSettings) -> Self {
        Self {
            model: s.model,
            reasoning_effort: s.reasoning_effort,
        }
    }
}

pub(crate) fn load(project_root: &Path) -> SubagentSettings {
    executor()
        .ok()
        .and_then(|rt| {
            rt.block_on(async {
                let client = crate::services::daemon_client::get_daemon_client().await?;
                client
                    .get_subagent_settings(GetSubagentSettingsRequest {
                        project_path: project_root.to_string_lossy().to_string(),
                    })
                    .await
            })
            .ok()
        })
        .map(SubagentSettings::from)
        .unwrap_or_default()
}

pub(crate) fn save(project_root: &Path, settings: &SubagentSettings) -> Result<(), String> {
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .set_subagent_settings(SetSubagentSettingsRequest {
                project_path: project_root.to_string_lossy().to_string(),
                settings: SubagentSettingsData {
                    model: settings.model.clone(),
                    reasoning_effort: settings.reasoning_effort,
                },
            })
            .await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_type_conversion() {
        let settings = SubagentSettings {
            model: Some("antigravity/gemini-3.1-pro".into()),
            reasoning_effort: Some(ReasoningEffort::High),
        };
        let data: SubagentSettingsData = settings.clone().into();
        let back: SubagentSettings = data.into();
        assert_eq!(settings, back);
    }
}
