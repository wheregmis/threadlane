use serde::{Deserialize, Serialize};

use crate::session::ReasoningEffort;

// ── Needle preferences ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetNeedleEnabledResponse {
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetNeedleEnabledRequest {
    pub enabled: bool,
}

// ── Subagent settings ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSubagentSettingsRequest {
    pub project_path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentSettingsData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetSubagentSettingsRequest {
    pub project_path: String,
    pub settings: SubagentSettingsData,
}
