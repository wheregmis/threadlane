use serde::{Deserialize, Serialize};
use threadlane_protocol::{AgentMessage, ReasoningEffort};

// Message, plan, usage, and tool-result contract types live in
// `threadlane-protocol` so provider, session, and UI layers share them
// without depending on the runtime; import them from there directly.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

/// Model-routing roles (`fast`, `fallback_chain`, `cooldown_models`).
/// Canonical in `threadlane_protocol::orchestration` — the daemon hydration
/// contract carries them — so existing `threadlane_runtime::ModelRoles`
/// paths keep working via this re-export.
pub use threadlane_protocol::orchestration::ModelRoles;

/// Orchestration mode: `Normal` direct execution or `Fusion` main +
/// sidekick routing.
/// Canonical in `threadlane_protocol::OrchestratorMode`; re-exported via the
/// `threadlane_protocol::{... OrchestratorMode}` import above so existing
/// `threadlane_protocol::OrchestratorMode` paths keep working.

#[derive(Debug, Clone)]
pub struct TurnState {
    pub system_prompt: String,
    pub messages: Vec<AgentMessage>,
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
    pub project_root: Option<std::path::PathBuf>,
}

impl TurnState {
    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    pub fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.reasoning_effort = effort;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_roles_resolve_fast_model_with_fallbacks() {
        let roles = ModelRoles {
            fast: Some("fast-model".into()),
            fallback_chain: vec!["primary".into(), "backup".into()],
            ..Default::default()
        };

        assert_eq!(roles.resolve_fast("base-model"), "fast-model");
        assert_eq!(roles.fallback_after("primary"), Some("backup"));
    }

    #[test]
    fn model_roles_are_backward_compatible_when_deserialized_without_fields() {
        let roles: ModelRoles = serde_json::from_str("{}").expect("default role config");
        assert_eq!(roles, ModelRoles::default());
        assert_eq!(roles.resolve_fast("base-model"), "base-model");
    }
}
