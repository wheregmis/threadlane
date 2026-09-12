//! Native computer-use tools: compatibility re-exports.
//!
//! The implementation is canonical in `threadlane_computer`, which owns
//! window introspection, screenshots, and input control behind the
//! `ComputerApproval` trait. The session implements that trait with its
//! permission manager (see `permission.rs`) and re-exports the crate below
//! for compatibility; new code should import `threadlane_computer` directly.

pub use threadlane_computer::{
    global_previews_dir, watch_display_for_debug, ComputerAct, ComputerCapability,
    ComputerToolExecutor, TargetedAct, COMPUTER_ACT_TOOL, COMPUTER_SCREENSHOT_TOOL,
    COMPUTER_STATUS_TOOL, COMPUTER_UNAVAILABLE, COMPUTER_WINDOWS_TOOL,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computer_tools_survive_core_schema_filter() {
        // Regression shape of the browser discovery bug: core_tool_schema_mode
        // strips every non-core schema from the provider payload.
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let agent =
            crate::coding_agent::CodingAgent::new(crate::coding_agent::CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "gpt-4o".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(session_file),
                system_prompt: Default::default(),
                agent_config: None,
                coding_config: None,
                browser: crate::browser::BrowserBridge::unavailable(),
            });
        let names: Vec<String> = agent
            .agent
            .configured_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        for tool in [
            COMPUTER_STATUS_TOOL,
            COMPUTER_WINDOWS_TOOL,
            COMPUTER_SCREENSHOT_TOOL,
            COMPUTER_ACT_TOOL,
        ] {
            assert!(
                names.iter().any(|name| name == tool),
                "model-visible schemas must include {tool}"
            );
        }
    }
}
