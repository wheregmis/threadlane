//! Harness-aware session runtime adapter for the GPUI frontend.

use std::sync::Arc;

use threadlane_session::CodingAgentOptions;

pub use threadlane_session::ExecutionMode;
pub use threadlane_session::SessionController as SessionRuntime;
pub use threadlane_session::SessionStatus as SessionRuntimeStatus;

/// Starts building a session runtime on the shared Tokio blocking pool and
/// returns the join handle so callers can overlap other work before awaiting.
///
/// `SessionRuntime::new` opens the durable harness and loads every WASI
/// extension for the project, running each module's `extension_info` through
/// the wasmi interpreter. GPUI's background executor runs its futures on GCD
/// worker threads with 512 KiB stacks, which is not enough headroom for that
/// constructor (it overflowed at startup while loading the bundled
/// extensions). Tokio's blocking threads get its 2 MiB default stack and carry
/// a reactor for anything the constructor spawns. Dropping the handle detaches
/// the task instead of cancelling it, so a hydration that is superseded still
/// finishes constructing, and then drops, its runtime.
pub(crate) fn spawn_session_runtime_construction(
    options: CodingAgentOptions,
    mode: ExecutionMode,
) -> tokio::task::JoinHandle<Arc<SessionRuntime>> {
    threadlane_runtime::get_runtime().spawn_blocking(move || SessionRuntime::new(options, mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use threadlane_session::CodingAgentOptions;

    #[test]
    fn runtime_opens_harness_in_the_canonical_session_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let work_dir = std::env::temp_dir().join(format!("threadlane-gpui-runtime-{unique}"));
        let session_file = work_dir.join(".threadlane/sessions/session.jsonl");
        let runtime = SessionRuntime::new(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "gpt-4o".into(),
                work_dir: work_dir.clone(),
                session_file: Some(session_file.clone()),
                system_prompt: Default::default(),
                agent_config: None,
                coding_config: None,
                browser: threadlane_session::BrowserBridge::unavailable(),
            },
            ExecutionMode::Interactive,
        );

        assert_eq!(runtime.session_file, session_file);
        assert!(runtime.session_file.exists());
        assert!(runtime.system_prompt.contains("Current working directory:"));
        assert!(!runtime
            .session_file
            .with_file_name("session.harness.jsonl")
            .exists());

        drop(runtime);
        let _ = std::fs::remove_dir_all(work_dir);
    }
}
