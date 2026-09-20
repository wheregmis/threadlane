//! Computer-use tools through the CUA driver: canonical implementation in
//! `threadlane_computer` (window introspection, accessibility snapshots,
//! screenshots, input control behind the `ComputerApproval` trait); engine
//! wiring (`Capability` → dispatcher) lives here so the computer crate stays
//! a leaf depending only on `threadlane-protocol`. Import leaf items from
//! `threadlane_computer` directly.

/// Runtime capability adapter for computer-use tools.
///
/// Lives in the engine (not `threadlane-computer`) so the computer crate
/// stays a leaf depending only on `threadlane-protocol`: it exposes the
/// executor, while engine wiring (`Capability` → dispatcher) stays here.
use threadlane_computer::ComputerApproval;
use threadlane_computer::computer::ComputerToolExecutor;

pub struct ComputerCapability {
    pub(crate) permissions: Option<std::sync::Arc<dyn ComputerApproval>>,
}

impl threadlane_runtime::Capability for ComputerCapability {
    fn id(&self) -> &str {
        "computer"
    }

    fn tool_executors(&self) -> Vec<std::sync::Arc<dyn threadlane_protocol::ToolExecutor>> {
        vec![std::sync::Arc::new(ComputerToolExecutor::new(
            self.permissions.clone(),
        ))]
    }
}

#[cfg(test)]
mod tests {
    use threadlane_computer::computer::{
        COMPUTER_ACT_TOOL, COMPUTER_AX_TOOL, COMPUTER_INTERACT_TOOL, COMPUTER_SCREENSHOT_TOOL,
        COMPUTER_STATUS_TOOL, COMPUTER_WINDOWS_TOOL, CUA_CALL_TOOL,
    };

    #[test]
    fn computer_tools_survive_core_schema_filter() {
        // Regression shape of the browser discovery bug: core_tool_schema_mode
        // strips every non-core schema from the provider payload.
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let agent = crate::runtime::CodingAgent::new(crate::options::CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(session_file),
            system_prompt: Default::default(),
            agent_config: None,
            coding_config: None,
            browser: threadlane_protocol::browser::BrowserBridge::unavailable(),
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
            COMPUTER_AX_TOOL,
            COMPUTER_INTERACT_TOOL,
            COMPUTER_ACT_TOOL,
            CUA_CALL_TOOL,
        ] {
            assert!(
                names.iter().any(|name| name == tool),
                "model-visible schemas must include {tool}"
            );
        }
    }
}
