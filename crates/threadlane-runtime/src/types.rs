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



#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelRoles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<String>,
    /// Ordered alternate models attempted after a pre-output quota/rate-limit failure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fallback_chain: Vec<String>,
    /// Persisted cooldown markers for temporarily exhausted provider/model routes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) cooldown_models: Vec<String>,
}

impl ModelRoles {
    pub fn resolve_fast<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.fast.as_deref().unwrap_or(fallback)
    }

    pub(crate) fn fallback_after<'a>(&'a self, current: &str) -> Option<&'a str> {
        self.fallback_chain
            .iter()
            .map(String::as_str)
            .find(|candidate| {
                *candidate != current
                    && !self
                        .cooldown_models
                        .iter()
                        .any(|cooldown| cooldown == candidate)
            })
    }
}

/// Orchestration mode governing explicit /prewalk engagement.
/// Canonical in `threadlane_protocol::OrchestratorMode`; re-exported via the
/// `threadlane_protocol::{... OrchestratorMode}` import above so existing
/// `threadlane_protocol::OrchestratorMode` paths keep working.

#[cfg(test)]
mod model_role_tests {
    use super::ModelRoles;

    #[test]
    fn fallback_skips_current_and_cooldown_routes() {
        let roles = ModelRoles {
            fallback_chain: vec!["primary".into(), "cooling".into(), "backup".into()],
            cooldown_models: vec!["cooling".into()],
            ..Default::default()
        };
        assert_eq!(roles.fallback_after("primary"), Some("backup"));
    }
}

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
