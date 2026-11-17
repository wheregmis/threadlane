//! Provider-neutral tool execution contracts.
//!
//! `ToolOutput` (text plus optional model-visible images) and the
//! `ToolExecutor` trait describe what a tool subsystem offers the agent loop,
//! independent of any execution engine. They live here so leaf crates
//! (`threadlane-computer`, `threadlane-wasi`, future tool crates) can
//! implement tools without depending on `threadlane-runtime`; the runtime
//! re-exports them for backward compatibility.

use crate::messages::{AgentToolCall, AgentToolDefinition, AgentToolResult, ImageAttachment};

/// Host-owned identity of a tool intent already committed to the session journal.
/// Call IDs alone are not unique across runs or lanes. This metadata is never
/// part of a model-visible tool schema or provider message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolExecutionIdentity {
    pub session_id: String,
    pub lane: String,
    pub run_id: String,
    pub assistant_entry_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub result_entry_id: String,
}

impl ToolExecutionIdentity {
    pub fn matches_call(&self, call_id: &str, tool_name: &str) -> bool {
        self.tool_call_id == call_id
            && self.tool_name == tool_name
            && [
                &self.session_id,
                &self.lane,
                &self.run_id,
                &self.assistant_entry_id,
                &self.tool_call_id,
                &self.tool_name,
                &self.result_entry_id,
            ]
            .iter()
            .all(|field| !field.trim().is_empty())
    }
}

/// A tool failure can be reported to the model. An unconfirmed durable
/// execution must stop the turn until the owning host reconciles its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolExecutionError {
    Failed(String),
    RecoveryRequired(String),
}

impl From<String> for ToolExecutionError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl std::fmt::Display for ToolExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(error) | Self::RecoveryRequired(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ToolExecutionError {}

pub enum RecoveredToolReply {
    Extension(ToolOutput),
    Canonical(AgentToolResult),
}

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

    /// Canonical dispatch entry point: preserves durable call identity,
    /// effective arguments, workspace context, and rich output together.
    /// Executors that need call identity override this; the default retains
    /// existing workspace/image-aware implementations.
    /// `identity` comes from committed host intent and is absent for legacy
    /// execution without a journal. For `dyn`, `call` names the resolved tool
    /// while the identity still names the model's original declaration.
    async fn execute_tool_with_call(
        &self,
        call: &AgentToolCall,
        args: &str,
        work_dir: Option<&std::path::Path>,
        _identity: Option<&ToolExecutionIdentity>,
    ) -> Option<Result<ToolOutput, ToolExecutionError>> {
        self.execute_tool_with_output_in_workspace(&call.name, args, work_dir)
            .await
            .map(|result| result.map_err(ToolExecutionError::Failed))
    }

    /// Retrieves a previously committed reply without executing the tool.
    /// Recovery can visit executors even when their old tool is no longer listed.
    async fn recover_tool_reply(
        &self,
        _call: &AgentToolCall,
        _args: &str,
        _work_dir: Option<&std::path::Path>,
        _identity: &ToolExecutionIdentity,
    ) -> Option<Result<RecoveredToolReply, ToolExecutionError>> {
        None
    }

    /// Saves the finished host reply before the canonical journal commit.
    async fn prepare_tool_reply(
        &self,
        _identity: &ToolExecutionIdentity,
        _result: &AgentToolResult,
    ) -> Result<(), ToolExecutionError> {
        Ok(())
    }

    /// Called only after the host commits the canonical tool result.
    async fn acknowledge_tool_reply(
        &self,
        _identity: &ToolExecutionIdentity,
    ) -> Result<(), String> {
        Ok(())
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

/// Extract the resolved file identity carried by a read-file snapshot.
pub fn read_file_snapshot_path(output: &str) -> Option<String> {
    output.lines().take(3).find_map(|line| {
        let path = line
            .strip_prefix("[Threadlane read_file path: ")?
            .strip_suffix(']')?;
        serde_json::from_str(path).ok()
    })
}
