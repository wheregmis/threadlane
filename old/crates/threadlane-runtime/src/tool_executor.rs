use threadlane_protocol::{AgentToolDefinition, ToolExecutor};
use std::sync::{Arc, OnceLock};
use threadlane_tools::{get_available_tools, try_execute_tool, try_execute_tool_in_workspace};

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

#[async_trait::async_trait]
impl ToolExecutor for BuiltinToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.builtin_tools"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        // Builtins are static literals; provider formats wrap the same list.
        // Extension executors retain their own live inventories.
        static DEFINITIONS: OnceLock<Arc<[AgentToolDefinition]>> = OnceLock::new();
        DEFINITIONS
            .get_or_init(|| {
                get_available_tools()
                    .into_iter()
                    .filter_map(|schema| AgentToolDefinition::from_provider_schema(&schema).ok())
                    .collect::<Vec<_>>()
                    .into()
            })
            .clone()
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

#[cfg(test)]
mod tests {
    use super::{BuiltinToolExecutor, ToolExecutor};
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn builtin_inventory_is_shared_and_preserves_both_provider_formats() {
        let first = BuiltinToolExecutor::new().tool_definitions();
        let second = BuiltinToolExecutor::new().tool_definitions();
        assert!(
            Arc::ptr_eq(&first, &second),
            "static builtin inventory must not be rebuilt per instance/batch"
        );
        for schemas in [
            threadlane_tools::get_available_tools(),
            threadlane_tools::get_codex_tools(),
        ] {
            let expected = schemas
                .iter()
                .map(threadlane_protocol::AgentToolDefinition::from_provider_schema)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(first.as_ref(), expected.as_slice());
        }
    }

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
