//! Shared Tokio reactor for provider background work.
//!
//! Provider model fetches must hop onto a reactor when the caller has none
//! (GPUI background tasks run without one, so a direct request would panic).
//! This process-wide runtime is that fallback; import it from here directly.

use std::sync::OnceLock;
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

/// Compatibility accessor for callers whose API cannot report initialization failure.
/// Interactive operations should use `try_get_runtime` instead.
pub fn get_runtime() -> &'static Runtime {
    try_get_runtime().expect("Failed to create Tokio runtime")
}

pub fn try_get_runtime() -> Result<&'static Runtime, String> {
    initialize_runtime(&RUNTIME, || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("threadlane-runtime")
            .build()
    })
}

fn initialize_runtime(
    cell: &OnceLock<Result<Runtime, String>>,
    build: impl FnOnce() -> std::io::Result<Runtime>,
) -> Result<&Runtime, String> {
    cell.get_or_init(|| build().map_err(|error| format!("Unable to start the model runtime: {error}")))
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_initialization_failure_is_returned_without_panicking() {
        let cell = OnceLock::new();
        let result = initialize_runtime(&cell, || Err(std::io::Error::other("injected failure")));
        assert!(result.unwrap_err().contains("injected failure"));
        assert!(initialize_runtime(&cell, || panic!("must reuse cached result")).is_err());
    }

    #[test]
    fn runtime_initialization_reuses_the_successful_runtime() {
        let cell = OnceLock::new();
        let first = initialize_runtime(&cell, || tokio::runtime::Builder::new_current_thread().build()).unwrap();
        let second = initialize_runtime(&cell, || panic!("must reuse runtime")).unwrap();
        assert!(std::ptr::eq(first, second));
    }
}
