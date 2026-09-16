//! Project-scoped subagent configuration shared by session and UI clients.

use serde::{Deserialize, Serialize};
use std::path::Path;
use threadlane_runtime::{OrchestratorMode, ReasoningEffort};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub orchestrator_mode: OrchestratorMode,
}

fn path(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".threadlane").join("subagents.json")
}

pub fn load(project_root: &Path) -> SubagentSettings {
    std::fs::read(path(project_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(project_root: &Path, settings: &SubagentSettings) -> Result<(), String> {
    if settings
        .reasoning_effort
        .is_some_and(|effort| ReasoningEffort::from_label(effort.label()).is_none())
        || settings
            .fast_reasoning_effort
            .is_some_and(|effort| ReasoningEffort::from_label(effort.label()).is_none())
    {
        return Err("Unsupported subagent reasoning effort.".into());
    }
    let target = path(project_root);
    let parent = target.parent().ok_or("Invalid subagent settings path.")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = target.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(temporary, target).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let settings = SubagentSettings {
            model: Some("antigravity/gemini-3.1-pro".into()),
            reasoning_effort: Some(ReasoningEffort::High),
            fast_model: Some("antigravity/gemini-3-flash".into()),
            fast_reasoning_effort: Some(ReasoningEffort::Low),
            orchestrator_mode: OrchestratorMode::Always,
        };
        save(dir.path(), &settings).unwrap();
        assert_eq!(load(dir.path()), settings);
    }
}
