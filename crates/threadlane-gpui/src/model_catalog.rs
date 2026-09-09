#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProvider {
    OpenAi,
    Antigravity,
    OpenCode,
    Acp,
}

impl ModelProvider {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Antigravity => "Antigravity",
            Self::OpenCode => "OpenCode",
            Self::Acp => "External agents",
        }
    }

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
        threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some(),
        threadlane_auth::opencode_auth::load_opencode_api_key().is_some(),
    );
    merge_registry_models(&mut models, project_root);
    merge_discovered_opencode_models(&mut models);
    append_acp_models(&mut models, project_root);
    models
}

/// Live-discovered Zen models, refreshed in the background by
/// [`refresh_discovered_models`]. Lets new `opencode-go/*` models appear in
/// the picker without a code change or a `models.json` entry.
static DISCOVERED_OPENCODE: std::sync::OnceLock<std::sync::Mutex<(std::time::Instant, Vec<ModelOption>)>> =
    std::sync::OnceLock::new();

fn pretty_bare_label(bare_id: &str) -> String {
    let mut label = String::new();
    for part in bare_id.split(['-', '_', '/']) {
        if part.is_empty() {
            continue;
        }
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
        bare_id.to_string()
    } else {
        label
    }
}

/// Fetches the live Zen model list and caches it for the picker. Skips the
/// network when there is no OpenCode key or the cache is still fresh.
pub async fn refresh_discovered_models() {
    if threadlane_auth::opencode_auth::load_opencode_api_key().is_none() {
        return;
    }
    let fresh = DISCOVERED_OPENCODE
        .get()
        .and_then(|cache| cache.lock().ok())
        .is_some_and(|guard| guard.0.elapsed() < std::time::Duration::from_secs(5 * 60));
    if fresh {
        return;
    }
    let mut discovered: Vec<ModelOption> =
        threadlane_provider::opencode::fetch_available_models()
            .await
            .into_iter()
            .map(|bare_id| ModelOption {
                id: format!("opencode-go/{bare_id}"),
                label: pretty_bare_label(&bare_id),
                provider: ModelProvider::OpenCode,
            })
            .collect();
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(cache) = DISCOVERED_OPENCODE.get_or_init(|| {
        std::sync::Mutex::new((std::time::Instant::now(), Vec::new()))
    })
    .lock()
    .ok()
    {
        let mut guard = cache;
        guard.0 = std::time::Instant::now();
        guard.1 = discovered;
    }
}

/// Refreshes the live Zen list, then rebuilds the picker's model list.
/// Call from a background task at startup and after the OpenCode key changes.
pub async fn refresh_discovered_models_and_update(
    model: gpui::Entity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    refresh_discovered_models().await;
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}

fn merge_discovered_opencode_models(models: &mut Vec<ModelOption>) {
    if !credentials_allow(ModelProvider::OpenCode) {
        return;
    }
    let discovered = DISCOVERED_OPENCODE
        .get()
        .and_then(|cache| cache.lock().ok())
        .map(|guard| guard.1.clone())
        .unwrap_or_default();
    for option in discovered {
        if !models.iter().any(|model| model.id == option.id) {
            models.push(option);
        }
    }
}

/// Launch-time snapshot of one external agent's settings, read without
/// opening a conversation. The picker serves these before any session's
/// engine connects, so switching sessions never re-asks the agent what it
/// offers. A failed agent keeps its error beside (possibly empty) options:
/// revalidation runs in the background and must not take away a previous
/// successful load.
#[derive(Clone, Debug)]
pub struct CachedAcpAgentModels {
    pub agent_id: String,
    pub agent_name: String,
    pub options: Vec<threadlane_session::AcpConfigOption>,
    pub error: Option<String>,
}

static CACHED_ACP_MODELS: std::sync::OnceLock<
    std::sync::Mutex<(std::time::Instant, Vec<CachedAcpAgentModels>)>,
> = std::sync::OnceLock::new();

/// Opens every enabled external agent once and caches the settings each
/// offers. Skips the spawns when the cache is still fresh; a failed agent
/// keeps its previous options with the fresh error attached.
pub async fn refresh_acp_models(project_root: Option<std::path::PathBuf>) {
    let fresh = CACHED_ACP_MODELS
        .get()
        .and_then(|cache| cache.lock().ok())
        .is_some_and(|guard| guard.0.elapsed() < std::time::Duration::from_secs(5 * 60));
    if fresh {
        return;
    }
    let manager = threadlane_session::AcpManager::new(
        threadlane_session::default_global_threadlane_dir(),
        project_root,
    );
    // ACP spawns agent subprocesses through `tokio::process`, which needs a
    // Tokio reactor. GPUI background tasks run on GPUI's own executor, so
    // hop onto the shared runtime and await the join handle back here.
    // A join failure leaves the previous cache (and its timestamp) alone.
    let preloaded = match threadlane_runtime::get_runtime()
        .spawn(async move { manager.preload_models().await })
        .await
    {
        Ok(preloaded) => preloaded,
        Err(error) => {
            tracing::warn!("external-agent model preload task failed: {error}");
            return;
        }
    };
    let mut cached: Vec<CachedAcpAgentModels> = preloaded
        .into_iter()
        .map(|preloaded| CachedAcpAgentModels {
            agent_id: preloaded.agent_id,
            agent_name: preloaded.agent_name,
            options: preloaded.options,
            error: preloaded.error,
        })
        .collect();
    cached.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    if let Some(cache) = CACHED_ACP_MODELS
        .get_or_init(|| std::sync::Mutex::new((std::time::Instant::now(), Vec::new())))
        .lock()
        .ok()
    {
        let mut guard = cache;
        guard.0 = std::time::Instant::now();
        guard.1 = cached;
    }
}

/// Refreshes the cached external-agent settings, then rebuilds the picker's
/// model list. Call from a background task at startup and whenever the
/// active project changes.
pub async fn refresh_acp_models_and_update(
    model: gpui::Entity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
    project_root: Option<std::path::PathBuf>,
) {
    refresh_acp_models(project_root).await;
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}

/// Cached settings for one external agent, if a launch-time (or later)
/// background refresh has seen it. Served even while a fresh validation is
/// failing: the picker must not lose models it already showed.
pub fn cached_acp_config_options(agent_id: &str) -> Vec<threadlane_session::AcpConfigOption> {
    CACHED_ACP_MODELS
        .get()
        .and_then(|cache| cache.lock().ok())
        .and_then(|guard| {
            guard
                .1
                .iter()
                .find(|cached| cached.agent_id == agent_id)
                .map(|cached| cached.options.clone())
        })
        .unwrap_or_default()
}

/// Latest background-validation error for one external agent, if the last
/// refresh saw it fail. The picker shows this instead of silently omitting
/// the agent's models; the models themselves keep coming from
/// [`cached_acp_config_options`].
pub fn cached_acp_error(agent_id: &str) -> Option<String> {
    CACHED_ACP_MODELS
        .get()
        .and_then(|cache| cache.lock().ok())
        .and_then(|guard| {
            guard
                .1
                .iter()
                .find(|cached| cached.agent_id == agent_id)
                .and_then(|cached| cached.error.clone())
        })
}

fn provider_for_id(id: &str, declared: Option<&str>) -> ModelProvider {
    match declared.unwrap_or_default().to_ascii_lowercase().as_str() {
        "openai" => ModelProvider::OpenAi,
        "antigravity" => ModelProvider::Antigravity,
        "opencode" | "opencode-go" => ModelProvider::OpenCode,
        "acp" => ModelProvider::Acp,
        _ if id.starts_with("antigravity/") => ModelProvider::Antigravity,
        _ if id.starts_with("opencode-go/") => ModelProvider::OpenCode,
        _ if id.starts_with("acp/") => ModelProvider::Acp,
        _ => ModelProvider::OpenAi,
    }
}

fn credentials_allow(provider: ModelProvider) -> bool {
    match provider {
        ModelProvider::OpenAi => has_openai_credentials(),
        ModelProvider::Antigravity => {
            threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some()
        }
        ModelProvider::OpenCode => {
            threadlane_auth::opencode_auth::load_opencode_api_key().is_some()
        }
        ModelProvider::Acp => true,
    }
}

/// Merges `models.json` registry entries (bundled, env, global, project) over
/// the compiled seeds so new models appear without a code change.
fn merge_registry_models(models: &mut Vec<ModelOption>, project_root: Option<&std::path::Path>) {
    for info in threadlane_runtime::model_registry::registry_for_project(project_root) {
        if let Some(existing) = models.iter_mut().find(|model| model.id == info.id) {
            if !info.label.trim().is_empty() {
                existing.label = info.label.clone();
            }
            continue;
        }
        // Skip ACP entries here; live agent configs own that section.
        if info.id.starts_with("acp/") {
            continue;
        }
        let provider = provider_for_id(&info.id, info.provider.as_deref());
        if provider != ModelProvider::Acp && !credentials_allow(provider) {
            continue;
        }
        models.push(ModelOption {
            id: info.id.clone(),
            label: if info.label.trim().is_empty() {
                info.id.clone()
            } else {
                info.label.clone()
            },
            provider,
        });
    }
}

pub(crate) fn efforts_for_model(
    model_id: &str,
    project_root: Option<&std::path::Path>,
) -> Vec<threadlane_runtime::ReasoningEffort> {
    threadlane_runtime::model_registry::supported_efforts_for(model_id, project_root)
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

fn append_acp_models(models: &mut Vec<ModelOption>, project_root: Option<&std::path::Path>) {
    let manager = threadlane_session::AcpManager::new(
        threadlane_session::default_global_threadlane_dir(),
        project_root.map(std::path::Path::to_path_buf),
    );
    for config in manager
        .configs()
        .into_iter()
        .filter(|config| config.enabled)
    {
        models.push(ModelOption {
            id: threadlane_session::acp_model_id(&config.id),
            label: config.name,
            provider: ModelProvider::Acp,
        });
    }
}

pub fn default_model() -> Option<String> {
    default_model_for_project(None)
}

pub(crate) fn default_model_for_project(project_root: Option<&std::path::Path>) -> Option<String> {
    available_models_for_project(project_root)
        .first()
        .map(|model| model.id.clone())
}

fn option_for(model_id: &str) -> Option<ModelOption> {
    provider_models(OPENAI_MODELS, ModelProvider::OpenAi)
        .into_iter()
        .chain(provider_models(
            ANTIGRAVITY_MODELS,
            ModelProvider::Antigravity,
        ))
        .chain(provider_models(OPENCODE_MODELS, ModelProvider::OpenCode))
        .find(|model| model.id == model_id)
}

pub(crate) fn label_for(model_id: &str) -> Option<String> {
    option_for(model_id).map(|model| model.label)
}

pub(crate) fn selection_label(model_id: &str, available: &[ModelOption]) -> String {
    available
        .iter()
        .find(|model| model.id == model_id)
        .map(|model| model.label.clone())
        .or_else(|| label_for(model_id))
        .unwrap_or_else(|| model_id.to_string())
}

pub fn available_option(model_id: &str) -> Option<ModelOption> {
    available_option_for_project(model_id, None)
}

fn available_option_for_project(
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

fn is_available_for_project(
    model_id: &str,
    project_root: Option<&std::path::Path>,
) -> bool {
    available_option_for_project(model_id, project_root).is_some()
}

fn has_openai_credentials() -> bool {
    let has_chatgpt_login =
        threadlane_auth::openai_auth::load_credentials().is_some_and(|credentials| {
            threadlane_auth::openai_auth::is_own_source(&credentials.source)
        });
    has_chatgpt_login
        || threadlane_auth::openai_auth::load_openai_api_key().is_some()
        || std::env::var("OPENAI_API_KEY").is_ok_and(|key| !key.trim().is_empty())
}

pub(crate) fn model_context_window(model: &str) -> u32 {
    threadlane_runtime::model_metadata::model_context_limit(model)
        .unwrap_or(threadlane_runtime::model_metadata::UNKNOWN_MODEL_CONTEXT_LIMIT)
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
    fn selection_labels_preserve_configured_agents_and_unavailable_models() {
        let models = vec![ModelOption {
            id: "acp/claude".into(),
            label: "Claude Code".into(),
            provider: ModelProvider::Acp,
        }];
        assert_eq!(selection_label("acp/claude", &models), "Claude Code");
        assert_eq!(selection_label("gpt-5.5", &models), "GPT-5.5");
        assert_eq!(selection_label("acp/removed-agent", &models), "acp/removed-agent");
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
            threadlane_runtime::model_metadata::UNKNOWN_MODEL_CONTEXT_LIMIT as u32,
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
    fn discovered_opencode_models_merge_without_duplicates() {
        assert_eq!(pretty_bare_label("deepseek-v4-flash"), "Deepseek V4 Flash");
        assert_eq!(pretty_bare_label("hy3"), "Hy3");
        let mut models = provider_models(OPENCODE_MODELS, ModelProvider::OpenCode);
        let before = models.len();
        // Seed entries already present must not be duplicated by discovery.
        if let Some(cache) = DISCOVERED_OPENCODE
            .get_or_init(|| std::sync::Mutex::new((std::time::Instant::now(), Vec::new())))
            .lock()
            .ok()
        {
            let mut guard = cache;
            guard.0 = std::time::Instant::now();
            guard.1 = vec![
                ModelOption {
                    id: "opencode-go/minimax-m2.7".into(),
                    label: "Minimax M2.7".into(),
                    provider: ModelProvider::OpenCode,
                },
                ModelOption {
                    id: "opencode-go/kimi-k2.6".into(),
                    label: "Kimi K2.6".into(),
                    provider: ModelProvider::OpenCode,
                },
            ];
        }
        merge_discovered_opencode_models(&mut models);
        assert_eq!(models.len(), before + 1);
        assert!(models.iter().any(|model| model.id == "opencode-go/kimi-k2.6"));
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
