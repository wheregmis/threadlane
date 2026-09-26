//! Regression: registered browser tools are model-visible by default.
//!
//! Relocated from `threadlane-session::browser` (compatibility shim) so the
//! session facade can be removed; the behavior under test belongs to the
//! coding-agent engine.

use threadlane_coding_agent::{CodingAgent, CodingAgentOptions};
use threadlane_protocol::browser::{
    BrowserBridge, BROWSER_ACT_TOOL, BROWSER_BACK_TOOL, BROWSER_CONSOLE_LOGS_TOOL,
    BROWSER_CURRENT_URL_TOOL, BROWSER_EVALUATE_TOOL, BROWSER_NAVIGATE_TOOL, BROWSER_RELOAD_TOOL,
    BROWSER_SCREENSHOT_TOOL, BROWSER_SNAPSHOT_TOOL, BROWSER_WAIT_TOOL,
};

#[test]
fn browser_tools_are_visible_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let session_file = dir.path().join("session.jsonl");
    let agent = CodingAgent::new(CodingAgentOptions {
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
