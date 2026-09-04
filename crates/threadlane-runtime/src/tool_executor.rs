use crate::types::{AgentToolCall, AgentToolDefinition};
use async_trait::async_trait;
use std::sync::Arc;
use threadlane_tools::{
    get_available_tools, get_codex_tools, try_execute_tool, try_execute_tool_in_workspace,
};

/// Executor for the built-in Threadlane tools.
///
/// Registering this alongside extension executors makes the dispatcher own one
/// ordered execution pipeline for every model-visible tool.
#[derive(Default)]
pub struct BuiltinToolExecutor;

impl BuiltinToolExecutor {
    fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolExecutor for BuiltinToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.builtin_tools"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        let mut seen = std::collections::HashSet::new();
        get_available_tools()
            .into_iter()
            .chain(get_codex_tools())
            .filter_map(|schema| AgentToolDefinition::from_provider_schema(&schema).ok())
            .filter(|definition| seen.insert(definition.name.clone()))
            .collect::<Vec<_>>()
            .into()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        Some(try_execute_tool(name, args))
    }

    async fn execute_tool_in_workspace(
        &self,
        name: &str,
        args: &str,
        work_dir: Option<&std::path::Path>,
    ) -> Option<Result<String, String>> {
        Some(match work_dir {
            Some(work_dir) => try_execute_tool_in_workspace(name, args, work_dir),
            None => try_execute_tool(name, args),
        })
    }
}

pub(crate) fn builtin_tool_executor() -> Arc<dyn ToolExecutor> {
    Arc::new(BuiltinToolExecutor::new())
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Stable identity used for deterministic registration and diagnostics.
    fn executor_id(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// Provider-neutral definitions for tools handled by this executor.
    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
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
}

#[cfg(test)]
mod tests {
    use super::{BuiltinToolExecutor, ToolExecutor};
    use tempfile::tempdir;

    #[tokio::test]
    async fn builtin_executor_preserves_tool_failure_status() {
        let executor = BuiltinToolExecutor::new();
        let result = executor
            .execute_tool("read_file", "{}")
            .await
            .expect("the built-in executor handles read_file");

        assert_eq!(result, Err("Error: 'path' parameter is required".into()));
    }

    #[tokio::test]
    async fn builtin_executor_preserves_nonzero_command_status() {
        let dir = tempdir().unwrap();
        let executor = BuiltinToolExecutor::new();
        let result = executor
            .execute_tool_in_workspace("run_command", r#"{"command":"exit 9"}"#, Some(dir.path()))
            .await
            .expect("the built-in executor handles run_command");

        let error = result.expect_err("a non-zero command exit must remain failed");
        assert!(error.contains("Exit Status: exit status: 9"), "{error}");
    }
}
