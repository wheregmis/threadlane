//! Model-scoped credential resolution for the session runtime.
//!
//! Provider selection is encoded in the persisted model id, but the signing
//! credential lives outside it: OpenAI-branch requests sign with an API key
//! or ChatGPT login token, while Antigravity/OpenCode branches resolve OAuth
//! internally per request. Any in-place model change (slash `/model`,
//! prewalk handoffs) must re-resolve through this function and rotate the
//! shared provider cell, or the new provider receives the previous
//! provider's credential (e.g. a Google `ya29` token sent to
//! `api.openai.com`, surfacing as 401 `invalid_api_key` mid-task).

use threadlane_protocol::ProviderPort;
use threadlane_provider::router::{is_antigravity_model, is_opencode_model};

/// True when `model` signs OpenAI-branch requests with the resolved pair.
/// Antigravity/OpenCode resolve OAuth internally per request and ACP never
/// touches the provider, so only this branch needs credential rotation.
pub fn uses_openai_credentials(model: &str) -> bool {
    !is_antigravity_model(model)
        && !is_opencode_model(model)
        && !crate::acp_bridge::is_acp_model(model)
}

/// Rotate the shared OpenAI-branch credential for `model` on an already
/// constructed provider client. Resolves through [`provider_credentials`];
/// no-ops when nothing usable resolves or the model never signs with this
/// pair. Idempotent: safe to call on every switch and handoff.
pub(crate) fn refresh_provider_for_model(
    provider: &std::sync::Arc<dyn ProviderPort>,
    model: &str,
) {
    if !uses_openai_credentials(model) {
        return;
    }
    let (key, account) = provider_credentials(model);
    if key.trim().is_empty() {
        return;
    }
    provider.refresh_openai_credentials(key, account);
}

/// Signing credential for `model`: `(api_key, account_id)`.
///
/// Precedence mirrors session construction: stored API key, own-source
/// ChatGPT login, environment. Antigravity/OpenCode models resolve OAuth
/// internally, so the returned pair is only meaningful for the OpenAI
/// branch — but resolving unconditionally keeps every switch path uniform.
pub fn provider_credentials(model: &str) -> (String, Option<String>) {
    if is_antigravity_model(model) {
        return (
            threadlane_provider::antigravity_auth::load_antigravity_credentials()
                .map(|credentials| credentials.access_token)
                .unwrap_or_default(),
            None,
        );
    }
    if is_opencode_model(model) {
        return (
            threadlane_auth::opencode_auth::load_opencode_api_key().unwrap_or_default(),
            None,
        );
    }
    if let Some(api_key) =
        threadlane_auth::openai_auth::load_openai_api_key().filter(|key| !key.trim().is_empty())
    {
        return (api_key, None);
    }
    if let Some(credentials) = threadlane_auth::openai_auth::load_credentials()
        .filter(|credentials| threadlane_auth::openai_auth::is_own_source(&credentials.source))
    {
        return (credentials.access_token, credentials.account_id);
    }
    (
        std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_openai_branch_signs_with_the_pair() {
        assert!(uses_openai_credentials("gpt-4o"));
        assert!(uses_openai_credentials("gpt-5.6-sol"));
        assert!(!uses_openai_credentials("antigravity/gemini-3.7-flash"));
        assert!(!uses_openai_credentials("opencode-go/minimax-m2.7"));
        assert!(!uses_openai_credentials("acp/claude"));
    }

    #[test]
    fn env_key_is_used_when_nothing_stored() {
        // Guard: on machines with real credentials the precedence differs,
        // so this hermetic check only runs in the fallback case.
        if threadlane_auth::openai_auth::load_openai_api_key().is_some()
            || threadlane_auth::openai_auth::load_credentials().is_some()
        {
            return;
        }
        let prior = std::env::var("OPENAI_API_KEY").ok();
        std::env::set_var("OPENAI_API_KEY", "test-env-key");
        let (key, account) = provider_credentials("gpt-4o");
        match prior {
            Some(value) => std::env::set_var("OPENAI_API_KEY", value),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
        assert_eq!(key, "test-env-key");
        assert_eq!(account, None);
    }

    #[test]
    fn non_openai_models_do_not_require_an_openai_key() {
        // Antigravity/OpenCode resolve OAuth internally; the pair only feeds
        // the OpenAI branch and must never gate those turns.
        let _ = provider_credentials("antigravity/gemini-3.7-flash");
        let _ = provider_credentials("opencode-go/minimax-m2.7");
    }
}
