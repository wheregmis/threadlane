//! Settings service: needle enabled preference and per-project subagent settings.

use std::path::Path;
use threadlane_protocol::project::default_global_threadlane_dir;
use threadlane_protocol::settings::*;
use threadlane_protocol::session::ReasoningEffort;

#[derive(Clone, Default)]
pub struct SettingsService;

impl SettingsService {
    pub fn new() -> Self {
        Self
    }

    // ── Needle ────────────────────────────────────────────────────────────

    fn needle_path() -> Option<std::path::PathBuf> {
        default_global_threadlane_dir().map(|d| d.join("gui").join("needle.json"))
    }

    pub fn get_needle_enabled(&self) -> Result<GetNeedleEnabledResponse, String> {
        let enabled = Self::needle_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<bool>(&b).ok())
            .unwrap_or(false);
        Ok(GetNeedleEnabledResponse { enabled })
    }

    pub fn set_needle_enabled(&self, req: SetNeedleEnabledRequest) -> Result<(), String> {
        let path = Self::needle_path()
            .ok_or_else(|| "Global settings directory unavailable".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec(&req.enabled).map_err(|e| e.to_string())?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }

    // ── Subagent settings ─────────────────────────────────────────────────

    fn subagent_path(project_root: &Path) -> std::path::PathBuf {
        project_root.join(".threadlane").join("subagents.json")
    }

    pub fn get_subagent_settings(
        &self,
        req: threadlane_protocol::settings::GetSubagentSettingsRequest,
    ) -> Result<SubagentSettingsData, String> {
        let project_root = Path::new(&req.project_path);
        let path = Self::subagent_path(project_root);
        let data: Option<SubagentSettingsData> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .filter(|s: &SubagentSettingsData| {
                matches!(
                    s.reasoning_effort,
                    None | Some(ReasoningEffort::Minimal)
                        | Some(ReasoningEffort::Low)
                        | Some(ReasoningEffort::Medium)
                        | Some(ReasoningEffort::High)
                )
            });
        Ok(data.unwrap_or_default())
    }

    pub fn set_subagent_settings(
        &self,
        req: threadlane_protocol::settings::SetSubagentSettingsRequest,
    ) -> Result<(), String> {
        if !matches!(
            req.settings.reasoning_effort,
            None | Some(ReasoningEffort::Minimal)
                | Some(ReasoningEffort::Low)
                | Some(ReasoningEffort::Medium)
                | Some(ReasoningEffort::High)
        ) {
            return Err("Unsupported subagent reasoning effort".into());
        }
        let project_root = Path::new(&req.project_path);
        let target = Self::subagent_path(project_root);
        let parent = target.parent().ok_or("Invalid subagent settings path")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let tmp = target.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&req.settings).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &target).map_err(|e| e.to_string())
    }
}
