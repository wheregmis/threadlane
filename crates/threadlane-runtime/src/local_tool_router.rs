use crate::types::AgentToolDefinition;
use std::collections::HashSet;

pub const NEEDLE_TOP_K: usize = 5;

pub fn render_needle_candidate(definition: &AgentToolDefinition) -> String {
    format!(
        "{}\n{}\n{}",
        definition.name,
        definition.description.as_deref().unwrap_or_default(),
        serde_json::to_string(&definition.parameters).unwrap_or_else(|_| "null".into())
    )
}

#[cfg_attr(not(feature = "needle"), allow(dead_code))]
fn definitions_for_ranks(
    ranked: &[(usize, f32)],
    definitions: &[AgentToolDefinition],
    max_tools: usize,
) -> Option<Vec<AgentToolDefinition>> {
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for (index, _) in ranked {
        let definition = definitions.get(*index)?;
        if seen.insert(*index) {
            selected.push(definition.clone());
        }
        if selected.len() == max_tools {
            break;
        }
    }
    (!selected.is_empty()).then_some(selected)
}

#[cfg(feature = "needle")]
static ENGINE: std::sync::OnceLock<Option<std::sync::Arc<needle_infer::v2_engine::V2Engine>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "needle")]
static IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "needle")]
struct InFlightGuard;

#[cfg(feature = "needle")]
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(feature = "needle")]
async fn run_with_inflight_gate<T, F>(
    timeout: std::time::Duration,
    job: F,
) -> Option<Result<Result<T, tokio::task::JoinError>, tokio::time::error::Elapsed>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if IN_FLIGHT.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return None;
    }
    let gate = InFlightGuard;
    Some(
        tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking(move || {
                let _gate = gate;
                job()
            }),
        )
        .await,
    )
}

#[cfg(feature = "needle")]
pub async fn shortlist_from_environment(
    query: &str,
    definitions: &[AgentToolDefinition],
    enabled: bool,
) -> Vec<AgentToolDefinition> {
    if !enabled {
        return definitions.to_vec();
    }
    if definitions.len() <= NEEDLE_TOP_K {
        return definitions.to_vec();
    };
    let Ok(engine) = needle_engine() else {
        return definitions.to_vec();
    };
    let query = query.to_owned();
    let definitions = definitions.to_vec();
    let fallback = definitions.clone();
    let started = std::time::Instant::now();
    let Some(result) = run_with_inflight_gate(std::time::Duration::from_secs(2), move || {
        let rendered = definitions
            .iter()
            .map(render_needle_candidate)
            .collect::<Vec<_>>();
        let descriptions = rendered.iter().map(String::as_str).collect::<Vec<_>>();
        let ranked = engine.retrieve_tools(&query, &descriptions, NEEDLE_TOP_K);
        definitions_for_ranks(&ranked, &definitions, NEEDLE_TOP_K)
            .unwrap_or_else(|| definitions.clone())
    })
    .await
    else {
        tracing::debug!(target: "threadlane_runtime::needle", duration_ms = started.elapsed().as_millis() as u64, "Needle routing already in flight; using full tool list");
        return fallback;
    };
    match result {
        Ok(Ok(routed)) => {
            let selected_names = routed
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>();
            tracing::debug!(target: "threadlane_runtime::needle", selected_tools = routed.len(), ?selected_names, duration_ms = started.elapsed().as_millis() as u64, "Needle routing completed");
            routed
        }
        Ok(Err(_)) => {
            tracing::warn!(target: "threadlane_runtime::needle", duration_ms = started.elapsed().as_millis() as u64, "Needle routing failed; using full tool list");
            fallback
        }
        Err(_) => {
            tracing::warn!(target: "threadlane_runtime::needle", duration_ms = started.elapsed().as_millis() as u64, "Needle routing timed out; using full tool list");
            fallback
        }
    }
}

#[cfg(feature = "needle")]
pub(crate) fn needle_model_path() -> std::path::PathBuf {
    std::env::var("THREADLANE_NEEDLE_WEIGHTS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../needle/needle2.cact")
        })
}

#[cfg(feature = "needle")]
pub(crate) fn needle_engine() -> Result<std::sync::Arc<needle_infer::v2_engine::V2Engine>, String> {
    let path = needle_model_path();
    if !path.is_file() {
        return Err("Needle model weights are unavailable.".into());
    }
    ENGINE
        .get_or_init(|| match needle_infer::v2_engine::V2Engine::load(&path) {
            Ok(engine) => Some(std::sync::Arc::new(engine)),
            Err(_) => None,
        })
        .clone()
        .ok_or_else(|| "Needle model weights could not be loaded.".into())
}

#[cfg(feature = "needle")]
pub fn validate_needle_model() -> Result<(), String> {
    needle_engine().map(|_| ())
}

#[cfg(not(feature = "needle"))]
pub fn validate_needle_model() -> Result<(), String> {
    Err("Needle support is not compiled into this build.".into())
}

#[cfg(not(feature = "needle"))]
pub async fn shortlist_from_environment(
    _query: &str,
    definitions: &[AgentToolDefinition],
    _enabled: bool,
) -> Vec<AgentToolDefinition> {
    definitions.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(feature = "needle")]
    static GATE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn tool(name: &str) -> AgentToolDefinition {
        AgentToolDefinition::new(name, name, json!({"type": "object"}))
    }

    #[test]
    fn maps_ranked_indexes_in_retrieval_order() {
        let definitions = vec![tool("alpha"), tool("beta"), tool("gamma")];
        assert_eq!(
            definitions_for_ranks(&[(2, 0.9), (0, 0.8), (2, 0.7)], &definitions, 5),
            Some(vec![tool("gamma"), tool("alpha")])
        );
    }

    #[test]
    fn rejects_any_out_of_range_rank() {
        let definitions = vec![tool("alpha"), tool("beta")];
        assert_eq!(
            definitions_for_ranks(&[(0, 1.0), (9, 0.5)], &definitions, 5),
            None
        );
    }

    #[test]
    fn renders_name_description_and_compact_parameters() {
        let rendered = render_needle_candidate(&AgentToolDefinition::new(
            "search_code",
            "Search workspace code.",
            json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        ));
        assert_eq!(
            rendered,
            "search_code\nSearch workspace code.\n{\"properties\":{\"query\":{\"type\":\"string\"}},\"type\":\"object\"}"
        );
    }

    #[tokio::test]
    async fn bypasses_needle_for_five_or_fewer_tools() {
        let definitions = (0..NEEDLE_TOP_K)
            .map(|i| tool(&format!("tool_{i}")))
            .collect::<Vec<_>>();
        assert_eq!(
            shortlist_from_environment("query", &definitions, true).await,
            definitions
        );
    }

    #[cfg(feature = "needle")]
    #[test]
    fn releases_inference_gate_when_guard_drops() {
        let _lock = GATE_TEST_LOCK.lock().unwrap();
        IN_FLIGHT.store(true, std::sync::atomic::Ordering::Release);
        {
            let _guard = InFlightGuard;
        }
        assert!(!IN_FLIGHT.load(std::sync::atomic::Ordering::Acquire));
    }

    #[cfg(feature = "needle")]
    #[tokio::test]
    async fn keeps_inference_gate_until_blocking_job_finishes() {
        use std::sync::mpsc;
        let _lock = GATE_TEST_LOCK.lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let job = run_with_inflight_gate(std::time::Duration::from_millis(1), move || {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
        })
        .await;
        started_rx.recv().unwrap();
        assert!(job.as_ref().is_some_and(Result::is_err));
        assert!(IN_FLIGHT.load(std::sync::atomic::Ordering::Acquire));
        finish_tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(!IN_FLIGHT.load(std::sync::atomic::Ordering::Acquire));
    }

    #[cfg(feature = "needle")]
    #[tokio::test]
    #[ignore = "loads the local 14 MB Needle model"]
    async fn needle_v2_retrieves_weather_in_top_five() {
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
            tool("search_code"),
            tool("read_file"),
            tool("write_file"),
            tool("list_directory"),
        ];
        let engine = needle_engine().unwrap();
        let rendered = definitions
            .iter()
            .map(render_needle_candidate)
            .collect::<Vec<_>>();
        let descriptions = rendered.iter().map(String::as_str).collect::<Vec<_>>();
        let ranked = engine.retrieve_tools(
            "what is the weather in Toronto?",
            &descriptions,
            NEEDLE_TOP_K,
        );
        let selected = definitions_for_ranks(&ranked, &definitions, NEEDLE_TOP_K).unwrap();
        assert!(selected.iter().any(|tool| tool.name == "get_weather"));
    }
}
