// ── SessionController & CodingAgent ──────────────────────────────────
use threadlane_coding_agent::CodingAgentOptions;
use threadlane_coding_agent::controller::{ExecutionMode, SessionController, SessionStatus};
pub type SessionRuntime = SessionController;
pub type SessionRuntimeStatus = SessionStatus;

pub fn runtime_status_text(status: SessionRuntimeStatus) -> Option<String> {
    match status {
        SessionRuntimeStatus::Ready => None,
        SessionRuntimeStatus::Working => Some("Working…".into()),
        SessionRuntimeStatus::Interrupted => {
            Some("Turn interrupted · Safe replay checkpoints available".into())
        }
        SessionRuntimeStatus::Error(error) => Some(error),
    }
}

/// Construct a session controller on the shared Tokio blocking pool. WASI
/// extension loading needs the larger stack and reactor provided there.
pub fn spawn_session_runtime_construction(
    options: CodingAgentOptions,
    mode: ExecutionMode,
) -> tokio::task::JoinHandle<std::sync::Arc<SessionController>> {
    threadlane_provider::exec::get_runtime().spawn_blocking(move || SessionController::new(options, mode))
}

/// Narrow adapters for cross-crate integration tests.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    use std::sync::Arc;

    use threadlane_protocol::ProviderPort;

    use threadlane_coding_agent::{CodingAgent, CodingAgentOptions};

    pub fn coding_agent_with_provider(
        options: CodingAgentOptions,
        provider: Arc<dyn ProviderPort>,
    ) -> CodingAgent {
        CodingAgent::new_with_provider(options, provider)
    }
}
