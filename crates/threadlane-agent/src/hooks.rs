use crate::types::{
    AfterToolCallResult, AgentMessage, AgentState, AgentToolCall, AgentToolDefinition,
    AgentToolResult, BeforeToolCallResult,
};
use async_trait::async_trait;

#[async_trait]
pub trait BeforeToolCallHook: Send + Sync {
    async fn before_tool_call(
        &self,
        tool_call: &AgentToolCall,
        state: &AgentState,
    ) -> BeforeToolCallResult;
}

#[async_trait]
pub trait AfterToolCallHook: Send + Sync {
    async fn after_tool_call(
        &self,
        tool_call: &AgentToolCall,
        result: &AgentToolResult,
        state: &AgentState,
    ) -> AfterToolCallResult;
}

#[async_trait]
pub trait TransformContextHook: Send + Sync {
    async fn transform_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage>;
}

#[async_trait]
pub trait ShouldStopAfterTurnHook: Send + Sync {
    async fn should_stop_after_turn(
        &self,
        turn_number: usize,
        tool_results: &[AgentToolResult],
        state: &AgentState,
    ) -> bool;
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Stable identity used for deterministic registration and diagnostics.
    fn executor_id(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// Provider-neutral definitions for tools handled by this executor.
    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        self.get_tool_schemas()
            .iter()
            .filter_map(|schema| AgentToolDefinition::from_provider_schema(schema).ok())
            .collect()
    }

    /// Legacy Chat Completions schemas. Prefer `tool_definitions` for new executors.
    fn get_tool_schemas(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>>;

    async fn execute_tool_with_call(
        &self,
        call: &AgentToolCall,
        args: &str,
    ) -> Option<Result<String, String>> {
        self.execute_tool(&call.name, args).await
    }
}
