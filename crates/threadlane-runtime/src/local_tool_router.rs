use crate::types::AgentToolDefinition;
#[cfg(any(test, feature = "needle"))]
use std::collections::HashSet;

/// Optional local shortlist stage. The provider remains authoritative.
#[cfg(feature = "needle")]
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalToolRouter {
    max_tools: usize,
}

#[cfg(feature = "needle")]
impl LocalToolRouter {
    fn new(max_tools: usize) -> Self {
        Self { max_tools }
    }

    fn shortlist(
        &self,
        selected_names: impl IntoIterator<Item = String>,
        definitions: &[AgentToolDefinition],
    ) -> Vec<AgentToolDefinition> {
        let selected: HashSet<String> = selected_names.into_iter().collect();
        filter_tool_definitions(&selected, definitions, self.max_tools)
    }
}

#[cfg(feature = "needle")]
pub(crate) async fn shortlist_from_environment(
    query: &str,
    definitions: &[AgentToolDefinition],
    enabled: bool,
) -> Vec<AgentToolDefinition> {
    if !enabled {
        return definitions.to_vec();
    }
    tracing::debug!(target: "threadlane_runtime::needle", "local Needle routing enabled");
    use std::sync::OnceLock;
    static ENGINE: OnceLock<Option<std::sync::Arc<needle_infer::v2_engine::V2Engine>>> =
        OnceLock::new();
    static IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some(engine) = ENGINE
        .get_or_init(|| {
            let path = std::env::var("THREADLANE_NEEDLE_WEIGHTS")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| default_model_path());
            match needle_infer::v2_engine::V2Engine::load(&path) {
                Ok(engine) => {
                    tracing::info!(target: "threadlane_runtime::needle", path = %path.display(), "Needle model loaded");
                    Some(std::sync::Arc::new(engine))
                }
                Err(error) => {
                    tracing::warn!(target: "threadlane_runtime::needle", path = %path.display(), %error, "Needle model unavailable; using full tool list");
                    None
                }
            }
        })
        .as_ref()
    else {
        return definitions.to_vec();
    };
    if IN_FLIGHT.swap(true, std::sync::atomic::Ordering::AcqRel) {
        tracing::debug!(target: "threadlane_runtime::needle", "Needle inference already running; using full tool list");
        return definitions.to_vec();
    }
    let engine = std::sync::Arc::clone(engine);
    let query = query.to_owned();
    let definitions = definitions.to_vec();
    let fallback = definitions.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            let tools = serde_json::to_string(&definitions).unwrap_or_default();
            let result = engine.generate(
                &query,
                &tools,
                &needle_infer::v2_engine::GenerateOptions {
                    max_new_tokens: 64,
                    constrain: true,
                    ..Default::default()
                },
                |_, _| {},
            );
            IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
            let selected = result
                .tool_call
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|value| {
                    value
                        .as_array()
                        .and_then(|calls| calls.first())
                        .or_else(|| value.as_object().map(|_| &value))
                        .and_then(|call| call.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
            LocalToolRouter::new(8).shortlist(selected.into_iter(), &definitions)
        }),
    )
    .await;
    match result {
        Ok(Ok(routed)) => {
            tracing::debug!(target: "threadlane_runtime::needle", selected_tools = routed.len(), "Needle routing completed");
            routed
        }
        Ok(Err(error)) => {
            tracing::warn!(target: "threadlane_runtime::needle", %error, "Needle inference failed; using full tool list");
            fallback
        }
        Err(_) => {
            tracing::warn!(target: "threadlane_runtime::needle", "Needle inference timed out; using full tool list");
            fallback
        }
    }
}

#[cfg(feature = "needle")]
fn default_model_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../needle/needle2.cact")
}

#[cfg(not(feature = "needle"))]
pub async fn shortlist_from_environment(
    _query: &str,
    definitions: &[AgentToolDefinition],
    _enabled: bool,
) -> Vec<AgentToolDefinition> {
    definitions.to_vec()
}

#[cfg(any(test, feature = "needle"))]
fn filter_tool_definitions(
    selected_names: &HashSet<String>,
    definitions: &[AgentToolDefinition],
    max_tools: usize,
) -> Vec<AgentToolDefinition> {
    if selected_names.is_empty() || max_tools == 0 {
        return definitions.to_vec();
    }
    let filtered: Vec<_> = definitions
        .iter()
        .filter(|definition| selected_names.contains(&definition.name))
        .take(max_tools)
        .cloned()
        .collect();
    if filtered.is_empty() {
        definitions.to_vec()
    } else {
        filtered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> AgentToolDefinition {
        AgentToolDefinition::new(name, name, json!({"type": "object"}))
    }

    #[test]
    fn filters_to_valid_selected_names() {
        let definitions = vec![tool("alpha"), tool("beta"), tool("gamma")];
        let selected = HashSet::from(["gamma".to_string(), "missing".to_string()]);
        assert_eq!(
            filter_tool_definitions(&selected, &definitions, 5),
            vec![tool("gamma")]
        );
    }

    #[test]
    fn falls_back_for_empty_or_invalid_selection() {
        let definitions = vec![tool("alpha"), tool("beta")];
        assert_eq!(
            filter_tool_definitions(&HashSet::new(), &definitions, 5),
            definitions
        );
        assert_eq!(
            filter_tool_definitions(&HashSet::from(["missing".to_string()]), &definitions, 5),
            definitions
        );
    }

    #[cfg(feature = "needle")]
    #[tokio::test]
    #[ignore = "loads the local 14 MB Needle model"]
    async fn needle_v2_selects_a_registered_tool() {
        let definitions = vec![
            AgentToolDefinition::new(
                "get_weather",
                "Get the current weather for a city.",
                json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
            ),
            AgentToolDefinition::new(
                "set_lights",
                "Control the lights in a room.",
                json!({"type": "object", "properties": {"room": {"type": "string"}, "state": {"type": "string"}}}),
            ),
        ];
        let engine = needle_infer::v2_engine::V2Engine::load(default_model_path()).unwrap();
        let result = engine.generate(
            "what is the weather in Toronto?",
            &serde_json::to_string(&definitions).unwrap(),
            &needle_infer::v2_engine::GenerateOptions {
                max_new_tokens: 64,
                constrain: true,
                ..Default::default()
            },
            |_, _| {},
        );
        let selected = result
            .tool_call
            .as_deref()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .and_then(|value| {
                value
                    .as_array()
                    .and_then(|calls| calls.first())
                    .or_else(|| value.as_object().map(|_| &value))
                    .and_then(|call| call.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        assert_eq!(selected.as_deref(), Some("get_weather"));
    }
}
