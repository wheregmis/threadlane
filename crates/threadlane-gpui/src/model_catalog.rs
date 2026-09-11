use std::collections::HashSet;

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
        threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some(),
        threadlane_auth::opencode_auth::load_opencode_api_key().is_some(),
    );
    merge_discovered_opencode_models(&mut models);
    merge_discovered_openai_models(&mut models);
    merge_registry_models(&mut models, project_root);
    append_acp_models(&mut models, project_root);
    retain_available_antigravity_models(&mut models, project_root);
    group_antigravity_models(&mut models);
    models
}

/// Live Antigravity entries can be appended after other providers. Keep the
/// provider contiguous so the picker renders one section heading.
fn group_antigravity_models(models: &mut [ModelOption]) {
    models.sort_by_key(|model| model.provider != ModelProvider::Antigravity);
}

/// Live-discovered Zen models, refreshed in the background by
/// [`refresh_discovered_models`]. Lets new `opencode-go/*` models appear in
/// the picker without a code change or a `models.json` entry.
static DISCOVERED_OPENCODE: std::sync::OnceLock<
    std::sync::Mutex<(std::time::Instant, Vec<ModelOption>)>,
> = std::sync::OnceLock::new();

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
    let mut discovered: Vec<ModelOption> = threadlane_provider::opencode::fetch_available_models()
        .await
        .into_iter()
        .map(|bare_id| ModelOption {
            id: format!("opencode-go/{bare_id}"),
            label: pretty_bare_label(&bare_id),
            provider: ModelProvider::OpenCode,
        })
        .collect();
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(cache) = DISCOVERED_OPENCODE
        .get_or_init(|| std::sync::Mutex::new((std::time::Instant::now(), Vec::new())))
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

/// Live `GET /v1/models` results, refreshed in the background by
/// [`refresh_openai_models`]. Merged additively like the Zen list: unknown
/// ids appear with generated labels. The last successful live result remains
/// available if a later refresh fails.
static DISCOVERED_OPENAI: std::sync::OnceLock<
    std::sync::Mutex<(
        std::time::Instant,
        Vec<ModelOption>,
        Vec<threadlane_runtime::model_registry::ModelInfo>,
    )>,
> = std::sync::OnceLock::new();

/// Pulls the live OpenAI model list and caches it for the picker. Skips the
/// network without credentials or while the cache is fresh; failures keep
/// the previous cache, and the next trigger retries once the TTL lapses.
pub async fn refresh_openai_models() {
    if !credentials_allow(ModelProvider::OpenAi) {
        return;
    }
    let fresh = DISCOVERED_OPENAI
        .get()
        .and_then(|cache| cache.lock().ok())
        .is_some_and(|guard| guard.0.elapsed() < std::time::Duration::from_secs(5 * 60));
    if fresh {
        return;
    }
    // Same precedence as session credential resolution: stored API key,
    // ChatGPT login, environment. A Codex-subscription token 401s on
    // `/v1/models`; subscription models arrive via the ChatGPT backend below.
    let (api_key, account_id) = crate::state::provider_credentials("gpt-4o");
    if api_key.trim().is_empty() {
        return;
    }
    let general =
        threadlane_provider::openai::try_fetch_available_models(&api_key, account_id.as_deref())
            .await;
    let subscription = threadlane_provider::openai::try_fetch_subscription_models().await;
    if general.is_none() && subscription.is_none() {
        return;
    }
    let cache = DISCOVERED_OPENAI
        .get_or_init(|| std::sync::Mutex::new((std::time::Instant::now(), Vec::new(), Vec::new())));
    let Ok(mut guard) = cache.lock() else {
        return;
    };
    if let Some(general) = general {
        guard.1 = general
            .into_iter()
            .map(|bare_id| ModelOption {
                id: bare_id.clone(),
                label: pretty_bare_label(&bare_id),
                provider: ModelProvider::OpenAi,
            })
            .collect();
    }
    if let Some(subscription) = subscription {
        threadlane_runtime::model_registry::update_discovered_models(
            "openai",
            subscription.clone(),
        );
        guard.2 = subscription;
    }
    guard.0 = std::time::Instant::now();
}

/// Refreshes the live OpenAI list, then rebuilds the picker's model list.
pub async fn refresh_openai_models_and_update(
    model: gpui::Entity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    refresh_openai_models().await;
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}

fn merge_discovered_openai_models(models: &mut Vec<ModelOption>) {
    if !credentials_allow(ModelProvider::OpenAi) {
        return;
    }
    let Some((mut discovered, subscription)) = DISCOVERED_OPENAI
        .get()
        .and_then(|cache| cache.lock().ok())
        .map(|guard| (guard.1.clone(), guard.2.clone()))
    else {
        return;
    };
    for info in subscription {
        discovered.retain(|model| model.id != info.id);
        discovered.push(ModelOption {
            id: info.id,
            label: info.label,
            provider: ModelProvider::OpenAi,
        });
    }
    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    for option in discovered {
        if !models.iter().any(|model| model.id == option.id) {
            models.push(option);
        }
    }
}

/// Live Antigravity runtime ids from `fetchAvailableModels`, refreshed in
/// the background by [`refresh_antigravity_models`]. `None` means no refresh
/// has ever succeeded: the static catalog stays authoritative rather than
/// wiping the picker on a failed fetch.
static DISCOVERED_ANTIGRAVITY: std::sync::OnceLock<
    std::sync::Mutex<(std::time::Instant, Option<HashSet<String>>)>,
> = std::sync::OnceLock::new();

/// Pulls the live Antigravity inventory for this account and caches the
/// runtime ids. Skips the network without stored credentials or while the
/// cache is fresh; an empty result keeps the previous success, and the next
/// trigger retries once the TTL lapses.
pub async fn refresh_antigravity_models() {
    if threadlane_provider::antigravity_auth::load_antigravity_credentials().is_none() {
        return;
    }
    let fresh = DISCOVERED_ANTIGRAVITY
        .get()
        .and_then(|cache| cache.lock().ok())
        .is_some_and(|guard| guard.0.elapsed() < std::time::Duration::from_secs(5 * 60));
    if fresh {
        return;
    }
    let live = threadlane_provider::antigravity::fetch_available_models().await;
    if live.is_empty() {
        return;
    }
    let ids = live.iter().map(|model| model.id.clone()).collect();
    let mut models = models_for_credentials(true, false);
    models.extend(
        threadlane_runtime::model_registry::registry_for_project(None)
            .into_iter()
            .filter(|model| model.id.starts_with("antigravity/"))
            .map(|model| ModelOption {
                id: model.id,
                label: model.label,
                provider: ModelProvider::Antigravity,
            }),
    );
    synthesize_live_antigravity_models(&mut models, &ids);
    threadlane_runtime::model_registry::update_discovered_models(
        "antigravity",
        antigravity_capabilities(&models, &live),
    );
    if let Some(cache) = DISCOVERED_ANTIGRAVITY
        .get_or_init(|| std::sync::Mutex::new((std::time::Instant::now(), None)))
        .lock()
        .ok()
    {
        let mut guard = cache;
        guard.0 = std::time::Instant::now();
        guard.1 = Some(ids);
    }
}

/// Refreshes the live Antigravity inventory, then rebuilds the picker's
/// model list.
pub async fn refresh_antigravity_models_and_update(
    model: gpui::Entity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    refresh_antigravity_models().await;
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}

/// Drops static Antigravity options whose runtime mapping no live refresh
/// has confirmed. A catalog entry survives when any of its supported
/// efforts resolves to a runtime id the backend advertises (e.g.
/// `gemini-3.7-flash` via `-tiered`); entries for retired models disappear
/// instead of failing at request time with 404/429.
/// True when any of the model's supported efforts resolves to a runtime id
/// the backend advertises. Pure so the availability rule is pinned without
/// network access or global-cache seeding.
fn antigravity_entry_available(
    model_id: &str,
    efforts: &[threadlane_runtime::ReasoningEffort],
    available: &HashSet<String>,
) -> bool {
    efforts.iter().any(|effort| {
        available.contains(&threadlane_provider::antigravity::runtime_model_for(
            model_id,
            &effort.label().to_ascii_lowercase(),
        ))
    })
}

fn retain_available_antigravity_models(
    models: &mut Vec<ModelOption>,
    project_root: Option<&std::path::Path>,
) {
    let available = live_antigravity_runtime_ids();
    if available.is_empty() {
        return;
    }
    models.retain(|model| {
        if model.provider != ModelProvider::Acp
            && threadlane_provider::router::is_antigravity_model(&model.id)
        {
            let efforts =
                threadlane_runtime::model_registry::supported_efforts_for(&model.id, project_root);
            antigravity_entry_available(&model.id, &efforts, &available)
        } else {
            true
        }
    });
    synthesize_live_antigravity_models(models, &available);
}

fn live_antigravity_runtime_ids() -> HashSet<String> {
    DISCOVERED_ANTIGRAVITY
        .get()
        .and_then(|cache| cache.lock().ok())
        .and_then(|guard| guard.1.clone())
        .unwrap_or_default()
}

/// Add live agent models that existing logical entries do not already route to.
/// Add tiered entries first so their per-effort variants don't become duplicate rows.
fn synthesize_live_antigravity_models(models: &mut Vec<ModelOption>, available: &HashSet<String>) {
    let mut runtime_ids: Vec<_> = available.iter().collect();
    runtime_ids.sort_by(|a, b| {
        b.ends_with("-tiered")
            .cmp(&a.ends_with("-tiered"))
            .then_with(|| a.cmp(b))
    });
    for runtime_id in runtime_ids {
        if models.iter().any(|model| {
            threadlane_runtime::model_registry::supported_efforts_for(&model.id, None)
                .iter()
                .any(|effort| {
                    threadlane_provider::antigravity::runtime_model_for(
                        &model.id,
                        &effort.label().to_ascii_lowercase(),
                    ) == *runtime_id
                })
        }) {
            continue;
        }
        let base = ["-tiered", "-low", "-medium", "-high"]
            .into_iter()
            .find_map(|suffix| runtime_id.strip_suffix(suffix))
            .unwrap_or(runtime_id);
        let logical_id = format!("antigravity/{base}");
        if models.iter().any(|model| model.id == logical_id) {
            continue;
        }
        let label = threadlane_runtime::model_registry::find_model(&logical_id, None)
            .map(|model| model.label)
            .unwrap_or_else(|| pretty_bare_label(base));
        models.push(ModelOption {
            id: logical_id,
            label,
            provider: ModelProvider::Antigravity,
        });
    }
}

fn antigravity_capabilities(
    models: &[ModelOption],
    live: &[threadlane_provider::antigravity::AntigravityModelInfo],
) -> Vec<threadlane_runtime::model_registry::ModelInfo> {
    models
        .iter()
        .filter_map(|model| {
            let mut supported_efforts = Vec::new();
            let mut display_name = None;
            for effort in threadlane_runtime::ReasoningEffort::known_levels() {
                let label = effort.label().to_ascii_lowercase();
                let runtime =
                    threadlane_provider::antigravity::runtime_model_for(&model.id, &label);
                if let Some(info) = live.iter().find(|info| info.id == runtime) {
                    if info.supported_efforts.contains(&label) {
                        supported_efforts.push(label);
                    }
                    if model.id.strip_prefix("antigravity/") == Some(info.id.as_str()) {
                        display_name = Some(info.display_name.clone());
                    }
                }
            }
            if supported_efforts.is_empty() {
                return None;
            }
            Some(threadlane_runtime::model_registry::ModelInfo {
                id: model.id.clone(),
                label: display_name.unwrap_or_else(|| model.label.clone()),
                provider: Some("antigravity".into()),
                context_window: None,
                default_effort: supported_efforts
                    .iter()
                    .find(|effort| effort.as_str() == "medium")
                    .or_else(|| supported_efforts.first())
                    .cloned(),
                supported_efforts,
            })
        })
        .collect()
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

/// Merges `models.json` metadata over the discovered catalog.
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
        // OpenAI inventory is account-specific and comes only from its live
        // model endpoints; registry data may annotate a discovered entry.
        if provider == ModelProvider::OpenAi {
            continue;
        }
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

/// Whether the reasoning-effort control applies to a model. ACP agents run
/// their own models (the native picker never reaches them), and registry
/// entries declaring only `off` have no thinking to tune — both hide the
/// control instead of offering dead options. Unknown models stay permissive
/// so new providers work before their registry entry lands.
pub(crate) fn supports_reasoning(model_id: &str, project_root: Option<&std::path::Path>) -> bool {
    // An unset model inherits its effort, so the control stays visible.
    if model_id.trim().is_empty() {
        return true;
    }
    if model_id.starts_with("acp/") {
        return false;
    }
    threadlane_runtime::model_registry::supported_efforts_for(model_id, project_root)
        != vec![threadlane_runtime::ReasoningEffort::Off]
}

fn models_for_credentials(has_antigravity: bool, has_opencode: bool) -> Vec<ModelOption> {
    let mut models = Vec::new();
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
    provider_models(ANTIGRAVITY_MODELS, ModelProvider::Antigravity)
        .into_iter()
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
        .unwrap_or_else(|| pretty_bare_label(model_id))
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

fn is_available_for_project(model_id: &str, project_root: Option<&std::path::Path>) -> bool {
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
        assert!(models_for_credentials(false, false).is_empty());
    }

    #[test]
    fn reasoning_support_hides_acp_and_off_only_models() {
        // ACP agents run their own models; the native picker never reaches them.
        assert!(!supports_reasoning("acp/claude", None));
        // Unset inherits, unknown stays permissive.
        assert!(supports_reasoning("", None));
        assert!(supports_reasoning("some-future/model", None));
        // Bundled registry data: non-reasoning vs reasoning models.
        assert!(!supports_reasoning("gpt-4o", None));
        assert!(!supports_reasoning("gpt-4o-mini", None));
        assert!(supports_reasoning("gpt-5.5", None));
        assert!(supports_reasoning("antigravity/gemini-3.7-flash", None));
    }

    #[test]
    fn antigravity_capabilities_follow_live_variants_and_disable_non_thinking() {
        use threadlane_provider::antigravity::AntigravityModelInfo;
        let models = vec![
            ModelOption {
                id: "antigravity/gemini-3.6-flash".into(),
                label: "Flash".into(),
                provider: ModelProvider::Antigravity,
            },
            ModelOption {
                id: "antigravity/test-no-thinking".into(),
                label: "Fast".into(),
                provider: ModelProvider::Antigravity,
            },
        ];
        let live = vec![
            AntigravityModelInfo {
                id: "gemini-3.6-flash-medium".into(),
                display_name: "Flash (Medium)".into(),
                supported_efforts: vec!["medium".into()],
                thinking_budget: Some(4000),
            },
            AntigravityModelInfo {
                id: "test-no-thinking".into(),
                display_name: "Fast".into(),
                supported_efforts: vec!["off".into()],
                thinking_budget: None,
            },
        ];
        let metadata = antigravity_capabilities(&models, &live);
        assert_eq!(metadata[0].supported_efforts, ["medium"]);
        assert_eq!(metadata[1].supported_efforts, ["off"]);
        threadlane_runtime::model_registry::update_discovered_models(
            "antigravity",
            vec![metadata[1].clone()],
        );
        assert!(!supports_reasoning("antigravity/test-no-thinking", None));
    }

    #[test]
    fn antigravity_availability_follows_live_runtime_ids() {
        use threadlane_runtime::ReasoningEffort;
        let available: HashSet<String> = ["gemini-3.7-flash-tiered".to_string()]
            .into_iter()
            .collect();
        // 3.7-flash resolves to -tiered at every effort.
        assert!(antigravity_entry_available(
            "antigravity/gemini-3.7-flash",
            &[ReasoningEffort::Low, ReasoningEffort::Medium],
            &available,
        ));
        // 3.6-flash medium/low/high exist only as suffixed ids.
        assert!(!antigravity_entry_available(
            "antigravity/gemini-3.6-flash",
            &[ReasoningEffort::Medium],
            &available,
        ));
        assert!(antigravity_entry_available(
            "antigravity/gemini-3.6-flash",
            &[ReasoningEffort::Medium],
            &["gemini-3.6-flash-medium".to_string()]
                .into_iter()
                .collect(),
        ));
    }

    /// Seeds the process-global Antigravity cache, restoring it afterwards
    /// so parallel tests never observe the fixture.
    fn with_antigravity_cache(stub: Option<HashSet<String>>, run: impl FnOnce()) {
        let saved = DISCOVERED_ANTIGRAVITY
            .get_or_init(|| std::sync::Mutex::new((std::time::Instant::now(), None)))
            .lock()
            .ok()
            .map(|guard| (guard.0, guard.1.clone()));
        if let Some(cache) = DISCOVERED_ANTIGRAVITY
            .get()
            .and_then(|cache| cache.lock().ok())
        {
            let mut guard = cache;
            guard.0 = std::time::Instant::now();
            guard.1 = stub;
        }
        run();
        if let (Some(saved), Some(cache)) = (
            saved,
            DISCOVERED_ANTIGRAVITY
                .get()
                .and_then(|cache| cache.lock().ok()),
        ) {
            let mut guard = cache;
            guard.0 = saved.0;
            guard.1 = saved.1;
        }
    }

    #[test]
    fn retired_antigravity_models_drop_out_once_confirmed() {
        with_antigravity_cache(
            Some(
                ["gemini-3.7-flash-tiered".to_string()]
                    .into_iter()
                    .collect(),
            ),
            || {
                let mut models = vec![
                    ModelOption {
                        id: "antigravity/gemini-3.7-flash".into(),
                        label: "Gemini 3.7 Flash".into(),
                        provider: ModelProvider::Antigravity,
                    },
                    ModelOption {
                        id: "antigravity/gemini-3.6-flash".into(),
                        label: "Gemini 3.6 Flash".into(),
                        provider: ModelProvider::Antigravity,
                    },
                    ModelOption {
                        id: "gpt-5.5".into(),
                        label: "GPT-5.5".into(),
                        provider: ModelProvider::OpenAi,
                    },
                ];
                retain_available_antigravity_models(&mut models, None);
                let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
                assert!(ids.contains(&"antigravity/gemini-3.7-flash"));
                assert!(!ids.contains(&"antigravity/gemini-3.6-flash"));
                assert!(ids.contains(&"gpt-5.5"));
            },
        );
    }

    #[test]
    fn unconfirmed_antigravity_catalog_stays_intact() {
        // No successful refresh yet: static list is authoritative.
        with_antigravity_cache(None, || {
            let mut models = vec![ModelOption {
                id: "antigravity/gemini-3.6-flash".into(),
                label: "Gemini 3.6 Flash".into(),
                provider: ModelProvider::Antigravity,
            }];
            retain_available_antigravity_models(&mut models, None);
            assert_eq!(models.len(), 1);
        });
    }

    #[test]
    fn live_tiered_generations_synthesize_missing_entries() {
        with_antigravity_cache(
            Some(
                [
                    "gemini-3.7-flash-tiered".to_string(),
                    "gemini-3.8-flash-tiered".to_string(),
                    "gemini-3.6-flash-medium".to_string(),
                ]
                .into_iter()
                .collect(),
            ),
            || {
                let mut models = vec![ModelOption {
                    id: "antigravity/gemini-3.6-flash".into(),
                    label: "Gemini 3.6 Flash".into(),
                    provider: ModelProvider::Antigravity,
                }];
                retain_available_antigravity_models(&mut models, None);
                let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
                // Static 3.6 row survives (medium confirmed); 3.8 appears
                // with a generated label; non-gemini runtime ids are ignored.
                assert!(ids.contains(&"antigravity/gemini-3.6-flash"));
                assert!(ids.contains(&"antigravity/gemini-3.8-flash"));
                let synthesized = models
                    .iter()
                    .find(|model| model.id == "antigravity/gemini-3.8-flash")
                    .unwrap();
                assert_eq!(synthesized.label, "Gemini 3.8 Flash");
                // Second pass is idempotent: no duplicate logical rows.
                retain_available_antigravity_models(&mut models, None);
                assert_eq!(
                    models
                        .iter()
                        .filter(|model| model.id == "antigravity/gemini-3.8-flash")
                        .count(),
                    1
                );
            },
        );
    }

    #[test]
    fn tiered_model_and_runtime_variants_share_one_new_picker_entry() {
        let mut models = Vec::new();
        synthesize_live_antigravity_models(
            &mut models,
            &[
                "gemini-3.6-flash-tiered".into(),
                "gemini-3.6-flash-low".into(),
                "gemini-3.6-flash-medium".into(),
                "gemini-3.6-flash-high".into(),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "antigravity/gemini-3.6-flash");
    }

    #[test]
    fn antigravity_models_share_one_picker_section() {
        let mut models = vec![
            ModelOption {
                id: "openai/model".into(),
                label: "OpenAI".into(),
                provider: ModelProvider::OpenAi,
            },
            ModelOption {
                id: "antigravity/first".into(),
                label: "First".into(),
                provider: ModelProvider::Antigravity,
            },
            ModelOption {
                id: "opencode/model".into(),
                label: "OpenCode".into(),
                provider: ModelProvider::OpenCode,
            },
            ModelOption {
                id: "antigravity/second".into(),
                label: "Second".into(),
                provider: ModelProvider::Antigravity,
            },
        ];
        group_antigravity_models(&mut models);
        assert_eq!(
            models
                .iter()
                .map(|model| model.provider)
                .collect::<Vec<_>>(),
            [
                ModelProvider::Antigravity,
                ModelProvider::Antigravity,
                ModelProvider::OpenAi,
                ModelProvider::OpenCode,
            ]
        );
    }

    #[test]
    fn selection_labels_preserve_configured_agents_and_unavailable_models() {
        let models = vec![ModelOption {
            id: "acp/claude".into(),
            label: "Claude Code".into(),
            provider: ModelProvider::Acp,
        }];
        assert_eq!(selection_label("acp/claude", &models), "Claude Code");
        assert_eq!(selection_label("gpt-5.5", &models), "Gpt 5.5");
        assert_eq!(
            selection_label("acp/removed-agent", &models),
            "acp/removed-agent"
        );
    }

    #[test]
    fn providers_only_expose_their_own_models() {
        assert!(
            models_for_credentials(true, false)
                .iter()
                .all(|model| model.provider == ModelProvider::Antigravity)
        );
        assert!(
            models_for_credentials(false, true)
                .iter()
                .all(|model| model.provider == ModelProvider::OpenCode)
        );
    }

    #[test]
    fn catalog_matches_native_provider_inventory() {
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
        assert!(
            models
                .iter()
                .any(|model| model.id == "opencode-go/kimi-k2.6")
        );
    }

    #[test]
    fn combined_catalog_preserves_provider_order() {
        let models = models_for_credentials(true, true);
        assert!(
            models[..ANTIGRAVITY_MODELS.len()]
                .iter()
                .all(|model| model.provider == ModelProvider::Antigravity)
        );
        assert!(
            models[ANTIGRAVITY_MODELS.len()..]
                .iter()
                .all(|model| model.provider == ModelProvider::OpenCode)
        );
    }

    #[test]
    fn openai_models_are_only_live_discovered() {
        assert!(models_for_credentials(false, false).is_empty());
    }
}
