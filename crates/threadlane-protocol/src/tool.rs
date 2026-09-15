//! Provider-neutral tool execution contracts.
//!
//! `ToolOutput` (text plus optional model-visible images) and the
//! `ToolExecutor` trait describe what a tool subsystem offers the agent loop,
//! independent of any execution engine. They live here so leaf crates
//! (`threadlane-computer`, `threadlane-wasi`, future tool crates) can
//! implement tools without depending on `threadlane-runtime`; the runtime
//! re-exports them for backward compatibility.

use crate::messages::{AgentToolCall, AgentToolDefinition, ImageAttachment};

/// Rich tool output: text plus optional model-visible images. Executors keep
/// returning plain strings; only image-producing tools build this directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub images: Vec<ImageAttachment>,
}

impl From<String> for ToolOutput {
    fn from(content: String) -> Self {
        Self {
            content,
            images: Vec::new(),
        }
    }
}

#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Stable identity used for deterministic registration and diagnostics.
    fn executor_id(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// Provider-neutral definitions for tools handled by this executor.
    fn tool_definitions(&self) -> std::sync::Arc<[AgentToolDefinition]> {
        self.get_tool_schemas()
            .iter()
            .filter_map(|schema| AgentToolDefinition::from_provider_schema(schema).ok())
            .collect::<Vec<_>>()
            .into()
    }

    /// Legacy Chat Completions schemas. Prefer `tool_definitions` for new executors.
    fn get_tool_schemas(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>>;

    /// Executes in the active workspace when the executor needs that context.
    /// The default preserves existing executors that do not use a workspace.
    async fn execute_tool_in_workspace(
        &self,
        name: &str,
        args: &str,
        _work_dir: Option<&std::path::Path>,
    ) -> Option<Result<String, String>> {
        self.execute_tool(name, args).await
    }

    async fn execute_tool_with_call(
        &self,
        call: &AgentToolCall,
        args: &str,
    ) -> Option<Result<String, String>> {
        self.execute_tool(&call.name, args).await
    }

    /// Rich variant carrying model-visible images alongside text. The default
    /// wraps the string result so existing executors stay untouched; only
    /// image-producing tools (screenshots) override this.
    async fn execute_tool_with_output_in_workspace(
        &self,
        name: &str,
        args: &str,
        work_dir: Option<&std::path::Path>,
    ) -> Option<Result<ToolOutput, String>> {
        self.execute_tool_in_workspace(name, args, work_dir)
            .await
            .map(|result| result.map(ToolOutput::from))
    }
}
