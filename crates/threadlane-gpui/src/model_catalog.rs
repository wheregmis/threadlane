#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProvider {
    OpenAi,
    Antigravity,
    OpenCode,
    Acp,
}

impl ModelProvider {
    pub(crate) const fn icon_path(self) -> &'static str {
        match self {
            Self::OpenAi => "icons/providers/openai.svg",
            Self::Antigravity => "icons/providers/google.svg",
            Self::OpenCode => "icons/providers/opencode.svg",
            Self::Acp => "icons/providers/acp.svg",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) provider: ModelProvider,
}

const OPENAI_MODELS: &[(&str, &str)] = &[
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
    ("gpt-5.4", "GPT-5.4"),
    ("gpt-5.4-mini", "GPT-5.4 Mini"),
    ("gpt-5.5", "GPT-5.5"),
    ("gpt-5.6-sol", "GPT-5.6 Sol"),
    ("gpt-5.6-terra", "GPT-5.6 Terra"),
    ("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
    ("gpt-4o", "GPT-4o"),
    ("gpt-4o-mini", "GPT-4o Mini"),
];

const ANTIGRAVITY_MODELS: &[(&str, &str)] = &[
    ("antigravity/gemini-3.7-flash", "Gemini 3.7 Flash"),
    ("antigravity/gemini-3.1-pro", "Gemini 3.1 Pro"),
    ("antigravity/claude-sonnet-4-6", "Claude Sonnet 4.6"),
    ("antigravity/claude-opus-4-6", "Claude Opus 4.6"),
    ("antigravity/gpt-oss-120b", "GPT-OSS 120B"),
];

const OPENCODE_MODELS: &[(&str, &str)] = &[
    ("opencode-go/mimo-v2.5-pro", "MiMo V2.5 Pro"),
    ("opencode-go/mimo-v2.5", "MiMo V2.5"),
    ("opencode-go/qwen3.8-max", "Qwen 3.8 Max"),
    ("opencode-go/minimax-m3", "MiniMax M3"),
    ("opencode-go/minimax-m2.7", "MiniMax M2.7"),
    ("opencode-go/deepseek-v4-pro", "DeepSeek V4 Pro"),
    ("opencode-go/deepseek-v4-flash", "DeepSeek V4 Flash"),
    ("opencode-go/hy3", "HY 3"),
];

fn provider_models(models: &[(&str, &str)], provider: ModelProvider) -> Vec<ModelOption> {
    models
        .iter()
        .map(|(id, label)| ModelOption {
            id: (*id).to_string(),
            label: (*label).to_string(),
            provider,
        })
        .collect()
}

pub fn available_models() -> Vec<ModelOption> {
    available_models_for_project(None)
}

pub(crate) fn available_models_for_project(
    project_root: Option<&std::path::Path>,
) -> Vec<ModelOption> {
    let mut models = models_for_credentials(
        has_openai_credentials(),
        crate::services::provider_auth::has_antigravity_credentials(),
        crate::services::provider_auth::has_opencode_credentials(),
    );
    append_acp_models(&mut models, project_root);
    models
}

fn models_for_credentials(
    has_openai: bool,
    has_antigravity: bool,
    has_opencode: bool,
) -> Vec<ModelOption> {
    let mut models = Vec::new();
    if has_openai {
        models.extend(provider_models(OPENAI_MODELS, ModelProvider::OpenAi));
    }
    if has_antigravity {
        models.extend(provider_models(
            ANTIGRAVITY_MODELS,
            ModelProvider::Antigravity,
        ));
    }
    if has_opencode {
        models.extend(provider_models(OPENCODE_MODELS, ModelProvider::OpenCode));
    }
    models
}

fn append_acp_models(_models: &mut Vec<ModelOption>, _project_root: Option<&std::path::Path>) {
}

pub fn default_model() -> Option<String> {
    default_model_for_project(None)
}

pub(crate) fn default_model_for_project(project_root: Option<&std::path::Path>) -> Option<String> {
    available_models_for_project(project_root)
        .first()
        .map(|model| model.id.clone())
}

pub fn option_for(model_id: &str) -> Option<ModelOption> {
    provider_models(OPENAI_MODELS, ModelProvider::OpenAi)
        .into_iter()
        .chain(provider_models(
            ANTIGRAVITY_MODELS,
            ModelProvider::Antigravity,
        ))
        .chain(provider_models(OPENCODE_MODELS, ModelProvider::OpenCode))
        .find(|model| model.id == model_id)
}

pub fn label_for(model_id: &str) -> Option<String> {
    option_for(model_id).map(|model| model.label)
}

pub fn available_option(model_id: &str) -> Option<ModelOption> {
    available_option_for_project(model_id, None)
}

pub(crate) fn available_option_for_project(
    model_id: &str,
    project_root: Option<&std::path::Path>,
) -> Option<ModelOption> {
    available_models_for_project(project_root)
        .into_iter()
        .find(|model| model.id == model_id)
}

pub fn is_available(model_id: &str) -> bool {
    is_available_for_project(model_id, None)
}

pub(crate) fn is_available_for_project(
    model_id: &str,
    project_root: Option<&std::path::Path>,
) -> bool {
    available_option_for_project(model_id, project_root).is_some()
}

fn has_openai_credentials() -> bool {
    crate::services::provider_auth::has_openai_credentials()
        || std::env::var("OPENAI_API_KEY").is_ok_and(|key| !key.trim().is_empty())
}

pub const UNKNOWN_MODEL_CONTEXT_LIMIT: usize = 128_000;

pub(crate) fn model_context_limit(model: &str) -> Option<usize> {
    if model.contains("gemini-3.1-pro") || model.contains("gemini-1.5-pro") {
        Some(2_000_000)
    } else if model.contains("flash") {
        Some(1_000_000)
    } else if model.contains("gpt-4o") || model.contains("o1") || model.contains("o3") {
        Some(128_000)
    } else {
        None
    }
}

pub(crate) fn model_context_window(model: &str) -> u32 {
    model_context_limit(model)
        .unwrap_or(UNKNOWN_MODEL_CONTEXT_LIMIT)
        .min(u32::MAX as usize) as u32
}

pub(crate) fn format_tokens(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_credentials_produce_no_provider_models() {
        assert!(models_for_credentials(false, false, false).is_empty());
    }

    #[test]
    fn providers_only_expose_their_own_models() {
        assert!(models_for_credentials(true, false, false)
            .iter()
            .all(|model| model.provider == ModelProvider::OpenAi));
        assert!(models_for_credentials(false, true, false)
            .iter()
            .all(|model| model.provider == ModelProvider::Antigravity));
        assert!(models_for_credentials(false, false, true)
            .iter()
            .all(|model| model.provider == ModelProvider::OpenCode));
    }

    #[test]
    fn catalog_matches_native_provider_inventory() {
        assert_eq!(OPENAI_MODELS.len(), 9);
        assert_eq!(ANTIGRAVITY_MODELS.len(), 5);
        assert_eq!(OPENCODE_MODELS.len(), 8);
    }

    #[test]
    fn context_window_and_token_formatting() {
        assert_eq!(
            model_context_window("antigravity/gemini-3.7-flash"),
            1_000_000
        );
        assert_eq!(
            model_context_window("unknown/model"),
            UNKNOWN_MODEL_CONTEXT_LIMIT as u32,
        );
        assert_eq!(
            model_context_window("antigravity/gemini-3.1-pro"),
            2_000_000
        );
        assert_eq!(format_tokens(850), "850");
        assert_eq!(format_tokens(24_500), "24.5k");
        assert_eq!(format_tokens(1_000_000), "1.0M");
    }

    #[test]
    fn combined_catalog_preserves_provider_order() {
        let models = models_for_credentials(true, true, true);
        assert!(models[..OPENAI_MODELS.len()]
            .iter()
            .all(|model| model.provider == ModelProvider::OpenAi));
        assert!(
            models[OPENAI_MODELS.len()..OPENAI_MODELS.len() + ANTIGRAVITY_MODELS.len()]
                .iter()
                .all(|model| model.provider == ModelProvider::Antigravity)
        );
        assert!(models[OPENAI_MODELS.len() + ANTIGRAVITY_MODELS.len()..]
            .iter()
            .all(|model| model.provider == ModelProvider::OpenCode));
    }
}
