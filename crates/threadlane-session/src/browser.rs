//! Agent tools for the embedded browser panel: compatibility re-exports.
//!
//! The session↔UI contract (`BrowserBridge`, `BrowserCommand`, `ActTarget`,
//! `BrowserRequest`, tool-name constants) is canonical in
//! `threadlane_protocol::browser`, and the tool executor
//! (`BrowserToolExecutor`) is canonical in `threadlane_runtime::browser`;
//! both are re-exported below for compatibility. New code should import from
//! those crates directly.

pub use threadlane_protocol::browser::{
    ActTarget, BrowserBridge, BrowserCommand, BrowserRequest, BROWSER_ACT_TOOL,
    BROWSER_BACK_TOOL, BROWSER_CONSOLE_LOGS_TOOL, BROWSER_CURRENT_URL_TOOL,
    BROWSER_EVALUATE_TOOL, BROWSER_NAVIGATE_TOOL, BROWSER_RELOAD_TOOL, BROWSER_SCREENSHOT_TOOL,
    BROWSER_SNAPSHOT_TOOL, BROWSER_UNAVAILABLE, BROWSER_WAIT_TOOL,
};
pub use threadlane_runtime::browser::BrowserToolExecutor;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_tools_survive_core_schema_filter() {
        // Regression: core_tool_schema_mode strips every non-core schema from
        // the provider payload. The browser tools must stay model-visible.
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
                browser: BrowserBridge::unavailable(),
            });
        let names: Vec<String> = agent
            .agent
            .configured_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        for tool in [
            BROWSER_NAVIGATE_TOOL,
            BROWSER_BACK_TOOL,
            BROWSER_RELOAD_TOOL,
            BROWSER_CURRENT_URL_TOOL,
            BROWSER_SNAPSHOT_TOOL,
            BROWSER_ACT_TOOL,
            BROWSER_EVALUATE_TOOL,
            BROWSER_SCREENSHOT_TOOL,
            BROWSER_CONSOLE_LOGS_TOOL,
            BROWSER_WAIT_TOOL,
        ] {
            assert!(
                names.iter().any(|name| name == tool),
                "model-visible schemas must include {tool}"
            );
        }
    }
}
