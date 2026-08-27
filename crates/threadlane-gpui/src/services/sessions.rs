//! Harness-aware session runtime adapter for the GPUI frontend.

pub use threadlane_session::ExecutionMode;
pub use threadlane_session::SessionController as SessionRuntime;
pub use threadlane_session::SessionStatus as SessionRuntimeStatus;

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
