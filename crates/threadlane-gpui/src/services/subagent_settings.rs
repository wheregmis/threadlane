use serde::{Deserialize, Serialize};
use std::path::Path;
use threadlane_runtime::ReasoningEffort;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SubagentSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fast_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fast_reasoning_effort: Option<ReasoningEffort>,
}

fn path(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".threadlane").join("subagents.json")
}

pub(crate) fn load(project_root: &Path) -> SubagentSettings {
    std::fs::read(path(project_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|settings: &SubagentSettings| {
            matches!(
                settings.reasoning_effort,
                None | Some(ReasoningEffort::Minimal)
                    | Some(ReasoningEffort::Low)
                    | Some(ReasoningEffort::Medium)
                    | Some(ReasoningEffort::High)
            ) && matches!(
                settings.fast_reasoning_effort,
                None | Some(ReasoningEffort::Minimal)
                    | Some(ReasoningEffort::Low)
                    | Some(ReasoningEffort::Medium)
                    | Some(ReasoningEffort::High)
            )
        })
        .unwrap_or_default()
}

pub(crate) fn save(project_root: &Path, settings: &SubagentSettings) -> Result<(), String> {
    if !matches!(
        settings.reasoning_effort,
        None | Some(ReasoningEffort::Minimal)
            | Some(ReasoningEffort::Low)
            | Some(ReasoningEffort::Medium)
            | Some(ReasoningEffort::High)
    ) {
        return Err("Unsupported subagent reasoning effort.".into());
    }
    if !matches!(
        settings.fast_reasoning_effort,
        None | Some(ReasoningEffort::Minimal)
            | Some(ReasoningEffort::Low)
            | Some(ReasoningEffort::Medium)
            | Some(ReasoningEffort::High)
    ) {
        return Err("Unsupported fast model reasoning effort.".into());
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
    fn settings_round_trip_and_missing_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), SubagentSettings::default());
        let settings = SubagentSettings {
            model: Some("antigravity/gemini-3.1-pro".into()),
            reasoning_effort: Some(ReasoningEffort::High),
            fast_model: Some("antigravity/gemini-3-flash".into()),
            fast_reasoning_effort: Some(ReasoningEffort::Low),
        };
        save(dir.path(), &settings).unwrap();
        assert_eq!(load(dir.path()), settings);
    }

    #[test]
    fn malformed_and_unsupported_settings_fall_back_safely() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".threadlane")).unwrap();
        std::fs::write(path(dir.path()), b"not-json").unwrap();
        assert_eq!(load(dir.path()), SubagentSettings::default());

        let unsupported = SubagentSettings {
            model: None,
            reasoning_effort: Some(ReasoningEffort::Max),
            fast_model: None,
            fast_reasoning_effort: None,
        };
        assert!(save(dir.path(), &unsupported).is_err());
    }
}
