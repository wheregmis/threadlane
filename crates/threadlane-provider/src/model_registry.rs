use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use threadlane_protocol::ReasoningEffort;

/// Live discovery freshness: background refresh triggers aim here, and
/// availability pruning trusts live data only within it. Expired entries
/// are still served (stale on failure) until a refresh replaces them.
pub const DISCOVERED_TTL: Duration = Duration::from_secs(5 * 60);

struct DiscoveredEntry {
    info: ModelInfo,
    discovered_at: Instant,
}

static DISCOVERED_MODELS: std::sync::OnceLock<std::sync::RwLock<HashMap<String, DiscoveredEntry>>> =
    std::sync::OnceLock::new();

pub fn pretty_model_label(id: &str) -> String {
    let mut label = String::new();
    for part in id.split(['-', '_', '/']).filter(|part| !part.is_empty()) {
        if !label.is_empty() {
            label.push(' ');
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            label.extend(first.to_uppercase());
            label.push_str(&chars.as_str().to_ascii_lowercase());
        }
    }
    if label.is_empty() {
        id.to_string()
    } else {
        label
    }
}

/// Publish successful provider discovery for both selectors and request adapters.
/// Failed/empty refreshes leave the last known capabilities intact.
pub fn update_discovered_models(provider: &str, models: Vec<ModelInfo>) {
    if models.is_empty() {
        return;
    }
    if let Ok(mut cache) = DISCOVERED_MODELS.get_or_init(Default::default).write() {
        let now = Instant::now();
        cache.retain(|_, entry| entry.info.provider.as_deref() != Some(provider));
        for model in models {
            cache.insert(
                model.id.clone(),
                DiscoveredEntry {
                    info: model,
                    discovered_at: now,
                },
            );
        }
    }
}

/// Snapshot of live-discovered models for registry merging: fresh and stale
/// alike. An empty or failed fetch never wipes (see
/// [`update_discovered_models`]), so expired entries stay visible until a
/// refresh replaces them.
fn discovered_snapshot() -> Vec<ModelInfo> {
    DISCOVERED_MODELS
        .get()
        .and_then(|cache| cache.read().ok())
        .map(|cache| cache.values().map(|entry| entry.info.clone()).collect())
        .unwrap_or_default()
}

/// True when live discovery is missing or older than [`DISCOVERED_TTL`]:
/// background refresh triggers consult this, and availability pruning
/// trusts live data only while fresh.
pub fn discovered_is_stale() -> bool {
    let Some(cache) = DISCOVERED_MODELS.get().and_then(|cache| cache.read().ok()) else {
        return true;
    };
    cache.is_empty()
        || cache
            .values()
            .any(|entry| entry.discovered_at.elapsed() > DISCOVERED_TTL)
}

/// A model entry that can be supplied without a code change.
///
/// Sources merge in increasing precedence:
/// 1. [`builtin_models()`] compiled seeds,
/// 2. `resources/models.json` if present (kept out of `target/`),
/// 3. `$THREADLANE_MODELS_JSON` file or inline JSON,
/// 4. `~/.threadlane/models.json`,
/// 5. `<project>/.threadlane/models.json`,
/// 6. live discovery ([`update_discovered_models`]): reasoning levels
///    overlay known ids and unknown ids append; labels and context windows
///    of curated seeds never change.
/// Later sources override earlier ones by `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

impl ModelInfo {
    pub(crate) fn efforts(&self) -> Vec<ReasoningEffort> {
        if self.supported_efforts.is_empty() {
            return ReasoningEffort::known_levels().to_vec();
        }
        let mut efforts = Vec::new();
        for raw in &self.supported_efforts {
            if let Some(effort) = ReasoningEffort::from_label(raw) {
                if !efforts.contains(&effort) {
                    efforts.push(effort);
                }
            }
        }
        if efforts.is_empty() {
            ReasoningEffort::known_levels().to_vec()
        } else {
            efforts
        }
    }
}

fn builtin_entry(id: &str, label: &str, provider: &str, context_window: usize) -> ModelInfo {
    ModelInfo {
        id: id.to_string(),
        label: label.to_string(),
        provider: Some(provider.to_string()),
        context_window: Some(context_window),
        supported_efforts: Vec::new(),
        default_effort: None,
    }
}

/// Compiled fallback seeds. Prefer `resources/models.json` or user files for
/// new models; this list only guarantees offline startup.
pub(crate) fn builtin_models() -> Vec<ModelInfo> {
    let mut models = Vec::new();
    for (id, label, context) in [
        ("gpt-5.6-luna", "GPT-5.6 Luna", 1_000_000),
        ("gpt-5.4", "GPT-5.4", 1_000_000),
        ("gpt-5.4-mini", "GPT-5.4 Mini", 128_000),
        ("gpt-5.5", "GPT-5.5", 1_000_000),
        ("gpt-5.6-sol", "GPT-5.6 Sol", 1_000_000),
        ("gpt-5.6-terra", "GPT-5.6 Terra", 1_000_000),
        ("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark", 128_000),
        ("gpt-4o", "GPT-4o", 128_000),
        ("gpt-4o-mini", "GPT-4o Mini", 128_000),
    ] {
        models.push(builtin_entry(id, label, "openai", context));
    }
    for (id, label, context) in [
        (
            "antigravity/gemini-3.7-flash",
            "Gemini 3.7 Flash",
            1_000_000,
        ),
        ("antigravity/gemini-3.1-pro", "Gemini 3.1 Pro", 2_000_000),
        (
            "antigravity/claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            200_000,
        ),
        ("antigravity/claude-opus-4-6", "Claude Opus 4.6", 200_000),
        ("antigravity/gpt-oss-120b", "GPT-OSS 120B", 128_000),
    ] {
        models.push(builtin_entry(id, label, "antigravity", context));
    }
    for (id, label) in [
        ("opencode-go/mimo-v2.5-pro", "MiMo V2.5 Pro"),
        ("opencode-go/mimo-v2.5", "MiMo V2.5"),
        ("opencode-go/qwen3.8-max", "Qwen 3.8 Max"),
        ("opencode-go/minimax-m3", "MiniMax M3"),
        ("opencode-go/minimax-m2.7", "MiniMax M2.7"),
        ("opencode-go/deepseek-v4-pro", "DeepSeek V4 Pro"),
        ("opencode-go/deepseek-v4-flash", "DeepSeek V4 Flash"),
        ("opencode-go/hy3", "HY 3"),
    ] {
        models.push(builtin_entry(id, label, "opencode", 128_000));
    }
    models
}

fn parse_models_value(value: serde_json::Value) -> Vec<ModelInfo> {
    if let Ok(models) = serde_json::from_value::<Vec<ModelInfo>>(value.clone()) {
        return models
            .into_iter()
            .filter(|model| !model.id.trim().is_empty())
            .collect();
    }
    if let Ok(wrapper) = serde_json::from_value::<HashMap<String, Vec<ModelInfo>>>(value) {
        if let Some(models) = wrapper.get("models") {
            return models
                .iter()
                .filter(|model| !model.id.trim().is_empty())
                .cloned()
                .collect();
        }
    }
    Vec::new()
}

pub(crate) fn load_models_from_file(path: &Path) -> Vec<ModelInfo> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .map(parse_models_value)
        .unwrap_or_default()
}

fn bundled_models() -> Vec<ModelInfo> {
    const BUNDLED: &str = include_str!("../../../resources/models.json");
    serde_json::from_str::<serde_json::Value>(BUNDLED)
        .map(parse_models_value)
        .unwrap_or_default()
}

fn env_models() -> Vec<ModelInfo> {
    let raw = std::env::var("THREADLANE_MODELS_JSON").unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        return parse_models_value(value);
    }
    load_models_from_file(Path::new(raw))
}

fn global_models_file() -> Vec<ModelInfo> {
    threadlane_project::default_global_threadlane_dir()
        .map(|dir| load_models_from_file(&dir.join("models.json")))
        .unwrap_or_default()
}

/// Merges model lists by `id`; later lists win and can augment labels,
/// context windows, and effort lists.
pub(crate) fn merge_models(lists: &[Vec<ModelInfo>]) -> Vec<ModelInfo> {
    let mut merged: HashMap<String, ModelInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for list in lists {
        for model in list {
            if !merged.contains_key(&model.id) {
                order.push(model.id.clone());
            }
            merged.insert(model.id.clone(), model.clone());
        }
    }
    order
        .into_iter()
        .filter_map(|id| merged.remove(&id))
        .collect()
}

/// Full registry for a project: builtins, bundled JSON, env, global,
/// project, plus live discovery merged additively.
///
/// Live entries never relabel curated seeds (label/context stay) and never
/// remove static entries: they overlay reasoning levels onto known ids and
/// append unknown ids, so new models appear with neither a code change nor
/// a `models.json` entry.
pub fn registry_for_project(project_root: Option<&Path>) -> Vec<ModelInfo> {
    let project_models = project_root
        .map(|root| load_models_from_file(&root.join(".threadlane").join("models.json")))
        .unwrap_or_default();
    let mut registry = merge_models(&[
        builtin_models(),
        bundled_models(),
        env_models(),
        global_models_file(),
        project_models,
    ]);
    for live in discovered_snapshot() {
        match registry.iter_mut().find(|model| model.id == live.id) {
            Some(existing) => {
                if !live.supported_efforts.is_empty() {
                    existing.supported_efforts = live.supported_efforts.clone();
                    existing.default_effort = live.default_effort.clone();
                }
            }
            None => registry.push(live),
        }
    }
    registry
}

/// Registry lookup by id across all file sources plus live discovery.
pub fn find_model(model_id: &str, project_root: Option<&Path>) -> Option<ModelInfo> {
    registry_for_project(project_root)
        .into_iter()
        .find(|model| model.id == model_id)
}

/// Preserve a valid selection; otherwise use the advertised default/first mode.
pub fn effective_effort(
    model_id: &str,
    effort: ReasoningEffort,
    project_root: Option<&Path>,
) -> ReasoningEffort {
    let Some(model) = find_model(model_id, project_root) else {
        return effort;
    };
    let supported = model.efforts();
    if supported.contains(&effort) {
        return effort;
    }
    model
        .default_effort
        .as_deref()
        .and_then(ReasoningEffort::from_label)
        .filter(|default| supported.contains(default))
        .unwrap_or(supported[0])
}

/// `none` is an explicit provider mode; `off` means omit the parameter entirely.
pub fn effective_api_effort(
    model_id: &str,
    effort: ReasoningEffort,
    project_root: Option<&Path>,
) -> Option<&'static str> {
    let effective = effective_effort(model_id, effort, project_root);
    if effective == ReasoningEffort::Off
        && find_model(model_id, project_root)
            .is_some_and(|model| model.supported_efforts.iter().any(|level| level == "none"))
    {
        Some("none")
    } else {
        effective.as_api_str()
    }
}

/// Supported efforts for a model id. Unknown models get all known levels so
/// new providers work before their registry entry lands.
pub fn supported_efforts_for(model_id: &str, project_root: Option<&Path>) -> Vec<ReasoningEffort> {
    find_model(model_id, project_root)
        .map(|model| model.efforts())
        .unwrap_or_else(|| ReasoningEffort::known_levels().to_vec())
}

/// Context-window override from dynamic registry files.
pub fn context_window_for(model_id: &str, project_root: Option<&Path>) -> Option<usize> {
    find_model(model_id, project_root).and_then(|model| model.context_window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_effort_round_trips_through_serde() {
        let effort = ReasoningEffort::from_label("ultra").unwrap();
        assert!(effort.is_custom());
        let json = serde_json::to_string(&effort).unwrap();
        assert_eq!(json, "\"ultra\"");
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>(&json).unwrap(),
            effort
        );
        assert_eq!(effort.as_api_str(), Some("ultra"));
    }

    #[test]
    fn later_registry_sources_override_by_id() {
        let base = vec![builtin_entry("a/model", "Base", "openai", 100)];
        let overlay = vec![ModelInfo {
            id: "a/model".into(),
            label: "Overlay".into(),
            provider: Some("openai".into()),
            context_window: Some(200),
            supported_efforts: vec!["high".into()],
            default_effort: None,
        }];
        let merged = merge_models(&[base, overlay]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].label, "Overlay");
        assert_eq!(merged[0].context_window, Some(200));
    }

    #[test]
    fn off_only_efforts_yield_off() {
        let info = ModelInfo {
            id: "test/no-reason".into(),
            label: "No Reason".into(),
            provider: None,
            context_window: None,
            supported_efforts: vec!["off".into()],
            default_effort: None,
        };
        assert_eq!(info.efforts(), vec![ReasoningEffort::Off]);
    }

    #[test]
    fn discovered_refresh_replaces_only_its_provider() {
        let model = |id: &str, provider: &str| ModelInfo {
            id: id.into(),
            label: id.into(),
            provider: Some(provider.into()),
            context_window: None,
            supported_efforts: vec!["low".into()],
            default_effort: None,
        };
        update_discovered_models("replace-test", vec![model("retired", "replace-test")]);
        update_discovered_models("other-test", vec![model("kept", "other-test")]);
        update_discovered_models("replace-test", vec![model("current", "replace-test")]);

        assert!(find_model("retired", None).is_none());
        assert!(find_model("current", None).is_some());
        assert!(find_model("kept", None).is_some());
    }

    #[test]
    fn file_models_parse_list_and_wrapped_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        std::fs::write(
            &path,
            r#"{"models": [{"id": "custom/model", "label": "Custom"}]}"#,
        )
        .unwrap();
        let models = load_models_from_file(&path);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "custom/model");
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    fn live(provider: &str, id: &str, label: &str, efforts: &[&str]) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            label: label.into(),
            provider: Some(provider.into()),
            context_window: Some(999),
            supported_efforts: efforts.iter().map(|effort| (*effort).into()).collect(),
            default_effort: None,
        }
    }

    #[test]
    fn discovered_models_merge_additively_without_relabeling_seeds() {
        update_discovered_models(
            "live-test-append",
            vec![live(
                "live-test-append",
                "live-test/brand-new",
                "Live Name",
                &["low"],
            )],
        );
        // Unknown ids append so new models reach the picker and payloads.
        let found = find_model("live-test/brand-new", None).expect("discovered model visible");
        assert_eq!(found.label, "Live Name");

        // Empty updates never wipe.
        update_discovered_models("live-test-append", Vec::new());
        assert!(find_model("live-test/brand-new", None).is_some());
    }

    #[test]
    fn discovered_levels_overlay_without_touching_curated_metadata() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".threadlane")).unwrap();
        std::fs::write(
            dir.path().join(".threadlane/models.json"),
            r#"[{"id":"live-test/seeded","label":"Curated","provider":"test","context_window":111,"supported_efforts":["off"],"default_effort":"off"}]"#,
        )
        .unwrap();
        update_discovered_models(
            "live-test-overlay",
            vec![live(
                "live-test-overlay",
                "live-test/seeded",
                "Upstream Relabel",
                &["low", "high"],
            )],
        );
        let merged = registry_for_project(Some(dir.path()))
            .into_iter()
            .find(|model| model.id == "live-test/seeded")
            .expect("seeded model present");
        // Levels come from live; label and context stay curated.
        assert_eq!(merged.supported_efforts, vec!["low", "high"]);
        assert_eq!(merged.label, "Curated");
        assert_eq!(merged.context_window, Some(111));
    }
}
