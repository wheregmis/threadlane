//! Shared Tokio reactor for provider background work.
//!
//! Provider model fetches must hop onto a reactor when the caller has none
//! (GPUI background tasks run without one, so a direct request would panic).
//! This process-wide runtime is that fallback; `threadlane-runtime`
//! re-exports [`get_runtime`] for backward compatibility.

use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("threadlane-runtime")
            .build()
            .expect("Failed to create Tokio runtime")
    })
}
