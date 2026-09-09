use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::types::ReasoningEffort;

/// A model entry that can be supplied without a code change.
///
/// Sources merge in increasing precedence:
/// 1. [`builtin_models()`] compiled seeds,
/// 2. `resources/models.json` if present (kept out of `target/`),
/// 3. `$THREADLANE_MODELS_JSON` file or inline JSON,
/// 4. `~/.threadlane/models.json`,
/// 5. `<project>/.threadlane/models.json`.
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
    pub fn efforts(&self) -> Vec<ReasoningEffort> {
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

fn builtin_entry(
    id: &str,
    label: &str,
    provider: &str,
    context_window: usize,
) -> ModelInfo {
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
pub fn builtin_models() -> Vec<ModelInfo> {
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

pub fn load_models_from_file(path: &Path) -> Vec<ModelInfo> {
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
    directories::BaseDirs::new()
        .map(|base| base.home_dir().join(".threadlane").join("models.json"))
        .map(|path| load_models_from_file(&path))
        .unwrap_or_default()
}

/// Merges model lists by `id`; later lists win and can augment labels,
/// context windows, and effort lists.
pub fn merge_models(lists: &[Vec<ModelInfo>]) -> Vec<ModelInfo> {
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

/// Full registry for a project: builtins, bundled JSON, env, global, project.
pub fn registry_for_project(project_root: Option<&Path>) -> Vec<ModelInfo> {
    let project_models = project_root
        .map(|root| load_models_from_file(&root.join(".threadlane").join("models.json")))
        .unwrap_or_default();
    merge_models(&[
        builtin_models(),
        bundled_models(),
        env_models(),
        global_models_file(),
        project_models,
    ])
}

/// Registry lookup by id across all file sources.
pub fn find_model(model_id: &str, project_root: Option<&Path>) -> Option<ModelInfo> {
    registry_for_project(project_root)
        .into_iter()
        .find(|model| model.id == model_id)
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
        assert_eq!(serde_json::from_str::<ReasoningEffort>(&json).unwrap(), effort);
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
