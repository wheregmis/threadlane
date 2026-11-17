pub mod antigravity_auth;
pub mod codex_auth;
pub mod github_auth;
pub mod openai_auth;
pub mod opencode_auth;
pub mod store;
pub mod traits;

pub use antigravity_auth::*;
pub use codex_auth::*;
pub use github_auth::*;
pub use openai_auth::*;
pub use opencode_auth::*;

use serde::de::DeserializeOwned;

fn parse_oauth_response<T: DeserializeOwned>(body: &str) -> Result<T, String> {
    serde_json::from_str(body).map_err(|_| "OAuth provider returned an invalid response".into())
}

/// Fallback Tokio reactor for auth network flows.
///
/// GPUI background tasks have no reactor, and hyper panics without one. The
/// UI boundary (`provider_auth`) already hops onto the provider runtime, but
/// every public network entry here hops too, so a future caller from a
/// reactor-less context cannot reintroduce the panic.
fn fallback_runtime() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("threadlane-auth")
            .enable_all()
            .build()
            .expect("auth fallback runtime")
    })
}

pub(crate) async fn on_reactor<Fut, T>(future: Fut) -> T
where
    Fut: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        future.await
    } else {
        fallback_runtime()
            .spawn(future)
            .await
            .expect("auth reactor task")
    }
}

#[cfg(test)]
fn test_env_guard_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};

    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_parse_errors_do_not_include_response_bodies() {
        let secret_body = r#"{"access_token":"secret-token""#;

        let error = parse_oauth_response::<serde_json::Value>(secret_body).unwrap_err();

        assert!(!error.contains("secret-token"));
        assert!(!error.contains(secret_body));
    }
}

#[cfg(test)]
mod reactor_tests {
    use super::*;

    #[test]
    fn network_entry_points_survive_without_an_ambient_reactor() {
        // Plain #[test] has no Tokio reactor: this is the GPUI-background
        // shape that used to panic inside hyper. block_on provides the outer
        // drive; on_reactor must hop the inner work onto the fallback.
        assert!(tokio::runtime::Handle::try_current().is_err());
        let outcome = fallback_runtime().block_on(on_reactor(async { "hopped".to_string() }));
        assert_eq!(outcome, "hopped");
    }
}
