//! Session-owned MCP adapter.
//!
//! The client implementation lives in [`threadlane_mcp`], which exposes
//! MCP-native tool metadata and structured results. This module re-exports it
//! so existing `crate::mcp::` paths keep working, and owns the one piece that
//! belongs to the host application: converting [`McpToolDescription`] into the
//! runtime [`AgentToolDefinition`] schema and flattening [`McpToolResult`]
//! into the text the model consumes.
//!
//! [`McpToolDescription`]: threadlane_mcp::McpToolDescription
//! [`McpToolResult`]: threadlane_mcp::McpToolResult
//! [`AgentToolDefinition`]: threadlane_runtime::AgentToolDefinition

pub use threadlane_mcp::*;

use std::sync::Arc;

use async_trait::async_trait;
use threadlane_runtime::{AgentToolDefinition, ToolExecutor};

/// Session-owned [`ToolExecutor`] over an [`McpManager`].
///
/// Tool definitions are projected from the manager's cached MCP-native
/// descriptions on every call, so they stay in sync with the latest
/// `discover_and_connect` without a second cache. Results are flattened with
/// [`McpToolResult::to_text`]; callers that need structured content use
/// [`McpManager::call_tool`] directly.
///
/// [`McpToolResult::to_text`]: threadlane_mcp::McpToolResult::to_text
pub struct McpToolExecutor {
    manager: Arc<McpManager>,
}

impl McpToolExecutor {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }

    pub fn manager(&self) -> &Arc<McpManager> {
        &self.manager
    }
}

/// Projects one MCP-native description onto the runtime tool schema.
pub fn mcp_tool_definition(description: &McpToolDescription) -> AgentToolDefinition {
    AgentToolDefinition::new(
        description.full_name.clone(),
        description.description.clone(),
        description.input_schema.clone(),
    )
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.mcp_tools.adapter"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        self.manager
            .tool_descriptions()
            .iter()
            .map(mcp_tool_definition)
            .collect::<Vec<_>>()
            .into()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        let parsed: serde_json::Value = match serde_json::from_str(args) {
            Ok(value) => value,
            Err(error) => return Some(Err(format!("Invalid JSON tool arguments: {error}"))),
        };
        match self.manager.call_tool(name, &parsed).await? {
            Ok(result) => Some(Ok(result.to_text())),
            Err(error) => Some(Err(error)),
        }
    }
}
