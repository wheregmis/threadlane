//! Agent Client Protocol (ACP) client support.
//!
//! Threadlane acts as an ACP *client*: it launches an external agent as a
//! subprocess and speaks newline-delimited JSON-RPC 2.0 over its stdio pipes.
//! The agent streams `session/update` notifications back and may call into the
//! client for filesystem access and tool permission decisions.
//!
//! This module owns the protocol and configuration layers only. It deliberately
//! contains no UI wiring: callers supply an [`AcpClientHandler`] and drive
//! [`AcpSession`] or [`AcpConnection`] directly.
//!
//! Configuration mirrors [`crate::mcp`]: `acp.json` in the global Threadlane
//! directory and in `<project>/.threadlane/`, with project entries shadowing
//! global entries that share an id.

use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};
use tokio::task::JoinHandle;

/// ACP major protocol version implemented by this client.
pub const ACP_PROTOCOL_VERSION: u16 = 1;

const ACP_SETTINGS_FILE: &str = "acp.json";
const MAX_CAPTURED_STDERR_BYTES: usize = 16 * 1024;
const ACP_PROJECT_SETTINGS_RELATIVE_PATH: &str = ".threadlane/acp.json";
const MAX_ACP_SETTINGS_BYTES: usize = 512 * 1024;

#[cfg(test)]
static LAST_SETTINGS_LOAD_THREAD: Mutex<Option<std::thread::ThreadId>> = Mutex::new(None);

#[cfg(test)]
fn last_settings_load_thread() -> Option<std::thread::ThreadId> {
    *LAST_SETTINGS_LOAD_THREAD.lock().ok()?
}
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpScope {
    #[default]
    Global,
    Project,
}

/// A configured external ACP agent.
///
/// ACP is defined over stdio only, so an agent is always a spawnable command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAgentConfig {
    pub id: String,
    pub name: String,
    command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    env: HashMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub scope: AcpScope,
}

fn default_enabled() -> bool {
    true
}

impl AcpAgentConfig {
    /// Builds a config from a display name and a single shell-style command
    /// line such as `npx -y @agentclientprotocol/claude-agent-acp`.
    ///
    /// Returns `None` when the name or command line is blank.
    pub fn from_command_line(name: &str, command_line: &str, scope: AcpScope) -> Option<Self> {
        let name = name.trim();
        let command_line = command_line.trim();
        if name.is_empty() || command_line.is_empty() {
            return None;
        }
        let mut parts = command_line.split_whitespace();
        let command = parts.next()?.to_string();
        let args: Vec<String> = parts.map(String::from).collect();
        Some(Self {
            id: slugify_id(name),
            name: name.to_string(),
            command,
            args,
            env: HashMap::new(),
            enabled: true,
            scope,
        })
    }

    /// Human-readable `command args...` summary for settings rows.
    pub fn command_line(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }
}

fn slugify_id(name: &str) -> String {
    let slug: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let slug = slug.trim_matches('_').to_string();
    if slug.is_empty() {
        "agent".to_string()
    } else {
        slug
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AcpSettingsFile {
    #[serde(default)]
    agents: Vec<AcpAgentConfig>,
}

/// Load/save helpers for `acp.json` at global and project scope.
pub struct AcpSettings;

impl AcpSettings {
    pub fn load_global(global_dir: Option<&Path>) -> Vec<AcpAgentConfig> {
        let Some(dir) = global_dir else {
            return Vec::new();
        };
        Self::load_file(&dir.join(ACP_SETTINGS_FILE), AcpScope::Global)
    }

    pub fn load_project(project_root: Option<&Path>) -> Vec<AcpAgentConfig> {
        let Some(root) = project_root else {
            return Vec::new();
        };
        Self::load_file(
            &root.join(ACP_PROJECT_SETTINGS_RELATIVE_PATH),
            AcpScope::Project,
        )
    }

    fn load_file(path: &Path, scope: AcpScope) -> Vec<AcpAgentConfig> {
        #[cfg(test)]
        if let Ok(mut thread) = LAST_SETTINGS_LOAD_THREAD.lock() {
            *thread = Some(std::thread::current().id());
        }
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
            Err(_) => return Vec::new(),
        };
        if bytes.len() > MAX_ACP_SETTINGS_BYTES {
            return Vec::new();
        }
        let parsed: AcpSettingsFile = match serde_json::from_slice(&bytes) {
            Ok(data) => data,
            Err(_) => return Vec::new(),
        };
        parsed
            .agents
            .into_iter()
            .map(|mut config| {
                config.scope = scope;
                config
            })
            .collect()
    }

    pub fn save_global(dir: &Path, agents: &[AcpAgentConfig]) -> Result<(), String> {
        Self::save_file(&dir.join(ACP_SETTINGS_FILE), agents)
    }

    pub fn save_project(root: &Path, agents: &[AcpAgentConfig]) -> Result<(), String> {
        Self::save_file(&root.join(ACP_PROJECT_SETTINGS_RELATIVE_PATH), agents)
    }

    fn save_file(file_path: &Path, agents: &[AcpAgentConfig]) -> Result<(), String> {
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings directory: {e}"))?;
        }
        let file_data = AcpSettingsFile {
            agents: agents.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file_data)
            .map_err(|e| format!("Failed to serialize ACP settings: {e}"))?;
        fs::write(file_path, bytes).map_err(|e| format!("Failed to write ACP settings: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Protocol types
// ---------------------------------------------------------------------------

/// Deserializes a value that must always succeed, falling back to the type's
/// default when the agent sends an enum variant this client does not know.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Same as [`lenient`], but for optional fields: an unknown variant becomes
/// `None` rather than failing the surrounding message.
fn lenient_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpImplementation {
    name: String,
    version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpFileSystemCapabilities {
    #[serde(default)]
    read_text_file: bool,
    #[serde(default)]
    write_text_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpClientCapabilities {
    #[serde(default)]
    fs: AcpFileSystemCapabilities,
    #[serde(default)]
    terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpPromptCapabilities {
    #[serde(default)]
    image: bool,
    #[serde(default)]
    audio: bool,
    #[serde(default)]
    embedded_context: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    prompt_capabilities: AcpPromptCapabilities,
}

impl AcpAgentCapabilities {
    /// Whether the agent accepts image blocks in a prompt.
    ///
    /// Agents that do not advertise this may reject the whole prompt, so an
    /// attachment has to be described in text instead of sent.
    pub fn supports_image_prompts(&self) -> bool {
        self.prompt_capabilities.image
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAuthMethod {
    pub id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpInitializeResult {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AcpAgentCapabilities,
    #[serde(default)]
    pub auth_methods: Vec<AcpAuthMethod>,
    #[serde(default)]
    agent_info: Option<AcpImplementation>,
}

impl AcpInitializeResult {
    /// Display name reported by the agent, falling back to a generic label.
    pub fn agent_display_name(&self) -> String {
        self.agent_info
            .as_ref()
            .map(|info| info.name.clone())
            .unwrap_or_else(|| "ACP agent".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSessionMode {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSessionModeState {
    current_mode_id: String,
    #[serde(default)]
    available_modes: Vec<AcpSessionMode>,
}

/// One choice offered by an [`AcpConfigOption`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConfigOptionChoice {
    pub value: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A session setting the agent exposes for the client to change.
///
/// This is how an agent surfaces things Threadlane has no protocol field for —
/// which model it runs, how much effort to spend, which permission mode is
/// active. The set is agent-defined and open-ended, so options are matched by
/// `id` and `category` rather than modelled as fixed fields.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConfigOption {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// Kept as raw JSON: `select` options carry strings today, but the type is
    /// agent-defined and a boolean or number must not fail the whole session.
    #[serde(default)]
    pub current_value: Value,
    #[serde(default)]
    pub options: Vec<AcpConfigOptionChoice>,
}

/// `category` an agent reports for the option naming its model.
pub const ACP_CONFIG_CATEGORY_MODEL: &str = "model";
/// `category` an agent reports for the option controlling its permission mode.
pub const ACP_CONFIG_CATEGORY_MODE: &str = "mode";
/// `category` an agent reports for the option controlling reasoning effort.
pub const ACP_CONFIG_CATEGORY_EFFORT: &str = "thought_level";
/// `id` of the agent-persona option, matched by id because it carries no
/// `category`.
pub const ACP_CONFIG_ID_AGENT: &str = "agent";

impl AcpConfigOption {
    pub fn current_value(&self) -> Option<&str> {
        self.current_value.as_str()
    }

    fn current_choice(&self) -> Option<&AcpConfigOptionChoice> {
        let current = self.current_value()?;
        self.options.iter().find(|choice| choice.value == current)
    }

    /// Name of the current selection, as the agent labels it.
    ///
    /// This is the control-sized label — "Default", "Plan Mode". Falls back to
    /// the raw value so an agent that reports something outside its own option
    /// list still displays truthfully.
    pub fn current_label(&self) -> Option<String> {
        self.current_choice()
            .map(|choice| choice.name.clone())
            .or_else(|| self.current_value().map(str::to_string))
    }

    /// The most specific description of the current selection.
    ///
    /// Prefers the leading segment of the agent's description, because that is
    /// where it names what is concretely running ("Opus 4.8 with 1M context ·
    /// Best for everyday…") — a model's option *name* is often generic
    /// ("Default (recommended)") and answers the wrong question. Only useful
    /// where there is room for a phrase; a mode's description is a whole
    /// sentence, so a button should use [`Self::current_label`] instead.
    pub fn current_detail_label(&self) -> Option<String> {
        self.current_description()
            .and_then(|description| description.split(" · ").next())
            .map(str::trim)
            .filter(|head| !head.is_empty())
            .map(str::to_string)
            .or_else(|| self.current_label())
    }

    /// Description the agent gave for the current selection, which is where it
    /// names the underlying model ("Opus 4.8 with 1M context").
    pub fn current_description(&self) -> Option<&str> {
        self.current_choice()
            .and_then(|choice| choice.description.as_deref())
    }

    pub fn has_choice(&self, value: &str) -> bool {
        self.options.iter().any(|choice| choice.value == value)
    }

    fn is_category(&self, category: &str) -> bool {
        self.category.as_deref() == Some(category)
    }

    /// Whether this option should be presented to the user for configuration.
    ///
    /// Excludes effort/thought level (owned by the reasoning picker) and
    /// agent persona (owned by the agent's internal routing).
    pub fn is_user_configurable(&self) -> bool {
        self.category.as_deref() != Some(ACP_CONFIG_CATEGORY_EFFORT)
            && self.id != ACP_CONFIG_ID_AGENT
    }
}

/// Finds the option an agent reports for `category`.
pub fn config_option_for<'a>(
    options: &'a [AcpConfigOption],
    category: &str,
) -> Option<&'a AcpConfigOption> {
    options.iter().find(|option| option.is_category(category))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpNewSessionResult {
    pub session_id: String,
    #[serde(default)]
    modes: Option<AcpSessionModeState>,
    #[serde(default)]
    config_options: Vec<AcpConfigOption>,
}

/// Response shape of `session/set_config_option`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcpSetConfigOptionResult {
    #[serde(default)]
    config_options: Vec<AcpConfigOption>,
}

/// A single block of prompt or response content.
///
/// Unknown block types deserialize to [`AcpContentBlock::Unknown`] so a newer
/// agent cannot break an in-flight turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpContentBlock {
    Text {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        data: String,
        mime_type: String,
    },
    #[serde(rename_all = "camelCase")]
    Audio {
        data: String,
        mime_type: String,
    },
    #[serde(rename_all = "camelCase")]
    ResourceLink {
        uri: String,
        #[serde(default)]
        name: Option<String>,
    },
    Resource {
        resource: Value,
    },
    #[serde(other)]
    Unknown,
}

impl AcpContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Plain-text projection used for transcript rendering.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpToolCallLocation {
    path: String,
    #[serde(default)]
    line: Option<u64>,
}

/// `tool_call` and `tool_call_update` payloads share one shape here; only
/// `toolCallId` is guaranteed present on an update.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpToolCall {
    pub(crate) tool_call_id: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub(crate) kind: Option<AcpToolKind>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub(crate) status: Option<AcpToolCallStatus>,
    #[serde(default)]
    pub(crate) content: Option<Vec<Value>>,
    #[serde(default)]
    locations: Option<Vec<AcpToolCallLocation>>,
    #[serde(default)]
    pub(crate) raw_input: Option<Value>,
    #[serde(default)]
    raw_output: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpPlanEntryPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpPlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AcpPlanEntry {
    pub(crate) content: String,
    priority: AcpPlanEntryPriority,
    pub(crate) status: AcpPlanEntryStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AcpAvailableCommand {
    name: String,
    #[serde(default)]
    description: String,
}

/// Decoded `session/update` payload.
///
/// The protocol keeps adding update kinds, so anything unrecognized is kept as
/// [`AcpSessionUpdate::Other`] instead of failing the whole notification.
#[derive(Debug, Clone, PartialEq)]
pub enum AcpSessionUpdate {
    UserMessageChunk(AcpContentBlock),
    AgentMessageChunk(AcpContentBlock),
    AgentThoughtChunk(AcpContentBlock),
    ToolCall(AcpToolCall),
    ToolCallUpdate(AcpToolCall),
    Plan(Vec<AcpPlanEntry>),
    AvailableCommandsUpdate(Vec<AcpAvailableCommand>),
    CurrentModeUpdate { current_mode_id: String },
    Other { kind: String, payload: Value },
}

impl AcpSessionUpdate {
    fn from_value(value: Value) -> Self {
        let kind = value
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        fn chunk(value: &Value) -> Option<AcpContentBlock> {
            serde_json::from_value(value.get("content")?.clone()).ok()
        }

        let decoded = match kind.as_str() {
            "user_message_chunk" => chunk(&value).map(Self::UserMessageChunk),
            "agent_message_chunk" => chunk(&value).map(Self::AgentMessageChunk),
            "agent_thought_chunk" => chunk(&value).map(Self::AgentThoughtChunk),
            "tool_call" => serde_json::from_value(value.clone())
                .ok()
                .map(Self::ToolCall),
            "tool_call_update" => serde_json::from_value(value.clone())
                .ok()
                .map(Self::ToolCallUpdate),
            "plan" => value
                .get("entries")
                .and_then(|entries| serde_json::from_value(entries.clone()).ok())
                .map(Self::Plan),
            "available_commands_update" => value
                .get("availableCommands")
                .and_then(|commands| serde_json::from_value(commands.clone()).ok())
                .map(Self::AvailableCommandsUpdate),
            "current_mode_update" => value
                .get("currentModeId")
                .and_then(Value::as_str)
                .map(|id| Self::CurrentModeUpdate {
                    current_mode_id: id.to_string(),
                }),
            _ => None,
        };

        decoded.unwrap_or(Self::Other {
            kind,
            payload: value,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcpSessionNotification {
    pub session_id: String,
    pub update: AcpSessionUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    #[default]
    Unknown,
}

impl AcpStopReason {
    /// Decodes a stop reason, treating an unrecognized value as
    /// [`AcpStopReason::Unknown`] rather than failing the turn.
    fn from_value(value: &Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpPermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPermissionOption {
    option_id: String,
    name: String,
    #[serde(default, deserialize_with = "lenient")]
    kind: AcpPermissionOptionKind,
}

impl AcpPermissionOption {
    pub fn option_id(&self) -> &str {
        &self.option_id
    }

    /// Agent-authored label for this choice ("Yes, allow once").
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> AcpPermissionOptionKind {
        self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPermissionRequest {
    session_id: String,
    #[serde(default)]
    tool_call: Option<AcpToolCall>,
    #[serde(default)]
    options: Vec<AcpPermissionOption>,
}

impl AcpPermissionRequest {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn tool_call(&self) -> Option<&AcpToolCall> {
        self.tool_call.as_ref()
    }

    pub fn options(&self) -> &[AcpPermissionOption] {
        &self.options
    }

    /// Picks the option matching `kind`, if the agent offered one.
    ///
    /// Agents choose their own option ids, so a decision can only ever be
    /// expressed as one of the ids they actually sent.
    pub fn option_for(&self, kind: AcpPermissionOptionKind) -> Option<&AcpPermissionOption> {
        self.options.iter().find(|option| option.kind == kind)
    }

    /// Whether the agent offered a durable "always" grant for this request.
    pub fn offers_allow_always(&self) -> bool {
        self.option_for(AcpPermissionOptionKind::AllowAlways)
            .is_some()
    }
}

/// Client answer to `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpPermissionOutcome {
    Selected { option_id: String },
    Cancelled,
}

impl AcpPermissionOutcome {
    fn to_json(&self) -> Value {
        match self {
            Self::Selected { option_id } => json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id,
                }
            }),
            Self::Cancelled => json!({ "outcome": { "outcome": "cancelled" } }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpReadTextFileRequest {
    session_id: String,
    path: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpWriteTextFileRequest {
    session_id: String,
    path: String,
    content: String,
}

// ---------------------------------------------------------------------------
// Client handler
// ---------------------------------------------------------------------------

/// Client-side half of ACP: everything the agent may ask Threadlane to do.
#[async_trait]
pub trait AcpClientHandler: Send + Sync {
    /// Called for each `session/update`, in the order the agent emitted them.
    ///
    /// This runs on the connection's read loop to preserve that order, so an
    /// implementation must hand the update off (a channel send, a queue push)
    /// rather than block on rendering or user interaction.
    async fn on_session_update(&self, notification: AcpSessionNotification);

    async fn request_permission(&self, request: AcpPermissionRequest) -> AcpPermissionOutcome;

    async fn read_text_file(&self, request: AcpReadTextFileRequest) -> Result<String, String>;

    async fn write_text_file(&self, request: AcpWriteTextFileRequest) -> Result<(), String>;
}

/// How an unattended client answers `session/request_permission`.
///
/// The default is [`AcpPermissionPolicy::Reject`]: without a UI in the loop
/// there is no informed consent, so nothing is auto-approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AcpPermissionPolicy {
    #[default]
    Reject,
    AllowOnce,
    AllowAlways,
}

impl AcpPermissionPolicy {
    fn select(&self, options: &[AcpPermissionOption]) -> AcpPermissionOutcome {
        let preferred: &[AcpPermissionOptionKind] = match self {
            Self::Reject => &[
                AcpPermissionOptionKind::RejectOnce,
                AcpPermissionOptionKind::RejectAlways,
            ],
            Self::AllowOnce => &[
                AcpPermissionOptionKind::AllowOnce,
                AcpPermissionOptionKind::AllowAlways,
            ],
            Self::AllowAlways => &[
                AcpPermissionOptionKind::AllowAlways,
                AcpPermissionOptionKind::AllowOnce,
            ],
        };
        for kind in preferred {
            if let Some(option) = options.iter().find(|option| option.kind == *kind) {
                return AcpPermissionOutcome::Selected {
                    option_id: option.option_id.clone(),
                };
            }
        }
        AcpPermissionOutcome::Cancelled
    }
}

/// Answers `session/request_permission` by asking somebody.
///
/// This is what lets a UI sit in the permission loop instead of the fixed
/// [`AcpPermissionPolicy`], which cannot represent "ask the user".
pub type AcpPermissionResponder = Arc<
    dyn Fn(AcpPermissionRequest) -> Pin<Box<dyn Future<Output = AcpPermissionOutcome> + Send>>
        + Send
        + Sync,
>;

/// Default handler: workspace-scoped filesystem access, a fixed permission
/// policy, and session updates forwarded on a channel.
pub struct AcpWorkspaceClient {
    workspace_root: PathBuf,
    permission_policy: AcpPermissionPolicy,
    permission_responder: Option<AcpPermissionResponder>,
    updates: Option<mpsc::UnboundedSender<AcpSessionNotification>>,
}

impl AcpWorkspaceClient {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            permission_policy: AcpPermissionPolicy::default(),
            permission_responder: None,
            updates: None,
        }
    }

    pub fn with_permission_policy(mut self, policy: AcpPermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    /// Routes permission requests to `responder` instead of the fixed policy.
    ///
    /// Takes precedence over [`Self::with_permission_policy`]: a responder is
    /// a real consent path, and the policy only exists for when there isn't
    /// one.
    pub fn with_permission_responder(mut self, responder: AcpPermissionResponder) -> Self {
        self.permission_responder = Some(responder);
        self
    }

    pub fn with_update_sender(
        mut self,
        sender: mpsc::UnboundedSender<AcpSessionNotification>,
    ) -> Self {
        self.updates = Some(sender);
        self
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        threadlane_tools::validate_path_in_workspace(path, &self.workspace_root)
    }
}

#[async_trait]
impl AcpClientHandler for AcpWorkspaceClient {
    async fn on_session_update(&self, notification: AcpSessionNotification) {
        if let Some(sender) = &self.updates {
            let _ = sender.send(notification);
        }
    }

    async fn request_permission(&self, request: AcpPermissionRequest) -> AcpPermissionOutcome {
        match &self.permission_responder {
            Some(responder) => responder(request).await,
            None => self.permission_policy.select(&request.options),
        }
    }

    async fn read_text_file(&self, request: AcpReadTextFileRequest) -> Result<String, String> {
        let path = self.resolve(&request.path)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read '{}': {e}", path.display()))?;

        // `line` is 1-based; `limit` counts lines from there.
        if request.line.is_none() && request.limit.is_none() {
            return Ok(content);
        }
        let start = request.line.unwrap_or(1).saturating_sub(1) as usize;
        let mut lines: Vec<&str> = content.lines().skip(start).collect();
        if let Some(limit) = request.limit {
            lines.truncate(limit as usize);
        }
        Ok(lines.join("\n"))
    }

    async fn write_text_file(&self, request: AcpWriteTextFileRequest) -> Result<(), String> {
        let path = self.resolve(&request.path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create '{}': {e}", parent.display()))?;
        }
        tokio::fs::write(&path, request.content)
            .await
            .map_err(|e| format!("Failed to write '{}': {e}", path.display()))
    }
}

/// Handler for connections that exist only to complete a handshake.
///
/// A probe has no session and no user watching, so it grants nothing: every
/// filesystem method is refused and every permission request is cancelled. This
/// is what keeps `AcpManager::discover_and_connect` from handing an unproven
/// third-party binary access to whatever directory the app happens to be in.
pub struct AcpProbeClient;

#[async_trait]
impl AcpClientHandler for AcpProbeClient {
    async fn on_session_update(&self, _notification: AcpSessionNotification) {}

    async fn request_permission(&self, _request: AcpPermissionRequest) -> AcpPermissionOutcome {
        AcpPermissionOutcome::Cancelled
    }

    async fn read_text_file(&self, _request: AcpReadTextFileRequest) -> Result<String, String> {
        Err("Filesystem access is not available while probing an ACP agent".to_string())
    }

    async fn write_text_file(&self, _request: AcpWriteTextFileRequest) -> Result<(), String> {
        Err("Filesystem access is not available while probing an ACP agent".to_string())
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

type PendingMap = HashMap<u64, oneshot::Sender<Result<Value, String>>>;
type PendingResponses = Arc<Mutex<PendingMap>>;
type BoxedWriter = Box<dyn AsyncWrite + Unpin + Send>;

fn lock_pending(pending: &PendingResponses) -> std::sync::MutexGuard<'_, PendingMap> {
    // A panic while dispatching must not poison the connection for good.
    pending.lock().unwrap_or_else(|error| error.into_inner())
}

/// A live JSON-RPC connection to one ACP agent process.
///
/// Dropping the connection kills the child process and fails any in-flight
/// requests.
pub struct AcpConnection {
    writer: Arc<TokioMutex<BoxedWriter>>,
    pending: PendingResponses,
    next_id: AtomicU64,
    reader_task: JoinHandle<()>,
    child: Option<TokioMutex<Child>>,
    stderr_task: Option<JoinHandle<Vec<u8>>>,
}

impl AcpConnection {
    /// Spawns `config` as a subprocess and drives ACP over its stdio pipes.
    async fn spawn(
        config: &AcpAgentConfig,
        cwd: Option<&Path>,
        handler: Arc<dyn AcpClientHandler>,
    ) -> Result<Self, String> {
        let mut command = Command::new(&config.command);
        command.args(&config.args);
        for (key, value) in &config.env {
            command.env(key, value);
        }
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|e| format!("Failed to spawn ACP agent '{}': {e}", config.command))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to open ACP agent stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to open ACP agent stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Failed to open ACP agent stderr".to_string())?;

        let mut connection = Self::from_streams(stdin, stdout, handler, Some(child));
        connection.stderr_task = Some(tokio::spawn(async move {
            let mut output = Vec::new();
            let _ = stderr
                .take(MAX_CAPTURED_STDERR_BYTES as u64)
                .read_to_end(&mut output)
                .await;
            output
        }));
        Ok(connection)
    }

    /// Builds a connection over arbitrary byte streams. Used by [`Self::spawn`]
    /// and by tests that pair the client with an in-process stub agent.
    pub fn from_streams<W, R>(
        writer: W,
        reader: R,
        handler: Arc<dyn AcpClientHandler>,
        child: Option<Child>,
    ) -> Self
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        let writer: Arc<TokioMutex<BoxedWriter>> = Arc::new(TokioMutex::new(Box::new(writer)));
        let pending: PendingResponses = Arc::new(Mutex::new(HashMap::new()));
        let reader_task = tokio::spawn(read_loop(
            BufReader::new(reader),
            Arc::clone(&pending),
            Arc::clone(&writer),
            handler,
        ));

        Self {
            writer,
            pending,
            next_id: AtomicU64::new(1),
            reader_task,
            child: child.map(TokioMutex::new),
            stderr_task: None,
        }
    }

    async fn send_line(&self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(message)
            .map_err(|e| format!("Failed to encode ACP message: {e}"))?;
        line.push('\n');
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to ACP agent: {e}"))?;
        writer
            .flush()
            .await
            .map_err(|e| format!("Failed to flush ACP agent stdin: {e}"))
    }

    /// Sends a request and awaits its response. `timeout` of `None` waits
    /// indefinitely, which is what a prompt turn needs.
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock_pending(&self.pending).insert(id, tx);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.send_line(&message).await {
            lock_pending(&self.pending).remove(&id);
            return Err(error);
        }

        let received = match timeout {
            Some(duration) => match tokio::time::timeout(duration, rx).await {
                Ok(received) => received,
                Err(_) => {
                    lock_pending(&self.pending).remove(&id);
                    return Err(format!("ACP request '{method}' timed out"));
                }
            },
            None => rx.await,
        };

        match received {
            Ok(result) => result,
            Err(_) => Err(format!(
                "ACP agent closed the connection while handling '{method}'"
            )),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send_line(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    pub async fn initialize(&self) -> Result<AcpInitializeResult, String> {
        let params = json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": false,
            },
            "clientInfo": {
                "name": "threadlane",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        let result = self
            .request("initialize", params, Some(HANDSHAKE_TIMEOUT))
            .await?;
        let initialized: AcpInitializeResult = serde_json::from_value(result)
            .map_err(|e| format!("Invalid ACP initialize response: {e}"))?;
        if initialized.protocol_version > ACP_PROTOCOL_VERSION {
            return Err(format!(
                "ACP agent requires protocol version {} but this client implements {ACP_PROTOCOL_VERSION}",
                initialized.protocol_version
            ));
        }
        Ok(initialized)
    }

    pub async fn authenticate(&self, method_id: &str) -> Result<(), String> {
        self.request(
            "authenticate",
            json!({ "methodId": method_id }),
            Some(HANDSHAKE_TIMEOUT),
        )
        .await
        .map(|_| ())
    }

    pub async fn new_session(
        &self,
        cwd: &Path,
        mcp_servers: Vec<Value>,
    ) -> Result<AcpNewSessionResult, String> {
        let params = json!({
            "cwd": cwd.to_string_lossy(),
            "mcpServers": mcp_servers,
        });
        let result = self
            .request("session/new", params, Some(HANDSHAKE_TIMEOUT))
            .await?;
        serde_json::from_value(result).map_err(|e| format!("Invalid ACP session/new response: {e}"))
    }

    /// Runs one prompt turn. Resolves when the agent reports a stop reason;
    /// use [`Self::cancel`] to interrupt it.
    pub async fn prompt(
        &self,
        session_id: &str,
        blocks: Vec<AcpContentBlock>,
    ) -> Result<AcpStopReason, String> {
        let params = json!({
            "sessionId": session_id,
            "prompt": blocks,
        });
        let result = self.request("session/prompt", params, None).await?;
        let stop_reason = result
            .get("stopReason")
            .ok_or_else(|| "ACP session/prompt response is missing stopReason".to_string())?;
        Ok(AcpStopReason::from_value(stop_reason))
    }

    pub async fn cancel(&self, session_id: &str) -> Result<(), String> {
        self.notify("session/cancel", json!({ "sessionId": session_id }))
            .await
    }

    /// Changes one agent-defined session setting, returning the full option
    /// set as the agent reports it afterwards.
    ///
    /// The reply is authoritative rather than assumed: changing one option can
    /// change another, as picking a different model changes which effort
    /// levels that model offers.
    pub async fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<AcpConfigOption>, String> {
        let result = self
            .request(
                "session/set_config_option",
                json!({ "sessionId": session_id, "configId": config_id, "value": value }),
                Some(HANDSHAKE_TIMEOUT),
            )
            .await?;
        Ok(serde_json::from_value::<AcpSetConfigOptionResult>(result)
            .map(|decoded| decoded.config_options)
            .unwrap_or_default())
    }

    pub async fn set_session_mode(&self, session_id: &str, mode_id: &str) -> Result<(), String> {
        self.request(
            "session/set_mode",
            json!({ "sessionId": session_id, "modeId": mode_id }),
            Some(HANDSHAKE_TIMEOUT),
        )
        .await
        .map(|_| ())
    }

    /// Terminates the agent process and stops the reader task.
    pub async fn shutdown(&self) {
        if let Some(child) = &self.child {
            let _ = child.lock().await.kill().await;
        }
        self.reader_task.abort();
        for (_, sender) in lock_pending(&self.pending).drain() {
            let _ = sender.send(Err("ACP connection was shut down".to_string()));
        }
    }
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        self.reader_task.abort();
        // `abort` only schedules cancellation, so the reader's own clone of the
        // pending map may outlive this call. Fail the waiters here instead of
        // relying on the channel senders being dropped at some later point.
        for (_, sender) in lock_pending(&self.pending).drain() {
            let _ = sender.send(Err("ACP connection was dropped".to_string()));
        }
    }
}

async fn read_loop<R>(
    mut reader: BufReader<R>,
    pending: PendingResponses,
    writer: Arc<TokioMutex<BoxedWriter>>,
    handler: Arc<dyn AcpClientHandler>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if method == "session/update" {
                // Session updates are order-sensitive: message chunks and
                // tool-call updates only reconstruct correctly in the order the
                // agent emitted them. Dispatching them inline preserves that
                // order; spawning a task per notification does not.
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                if let Some(notification) = parse_session_notification(params) {
                    handler.on_session_update(notification).await;
                }
            } else {
                // Requests can block on a user decision, so they must not stall
                // the read loop the way an inline notification does.
                tokio::spawn(dispatch_request(
                    message,
                    Arc::clone(&writer),
                    Arc::clone(&handler),
                ));
            }
            continue;
        }

        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let Some(sender) = lock_pending(&pending).remove(&id) else {
            continue;
        };
        let payload = if let Some(error) = message.get("error") {
            Err(format_rpc_error(error))
        } else {
            Ok(message.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = sender.send(payload);
    }

    for (_, sender) in lock_pending(&pending).drain() {
        let _ = sender.send(Err("ACP agent closed the connection".to_string()));
    }
}

fn format_rpc_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("ACP agent returned an error");
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => format!("{message} (code {code})"),
        None => message.to_string(),
    }
}

/// Handles one agent-initiated request and writes its response.
async fn dispatch_request(
    message: Value,
    writer: Arc<TokioMutex<BoxedWriter>>,
    handler: Arc<dyn AcpClientHandler>,
) {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    // A method without an id is a notification we do not implement; there is
    // nothing to answer.
    let Some(id) = message.get("id").cloned() else {
        return;
    };

    let outcome: Result<Value, (i64, String)> = match method.as_str() {
        "session/request_permission" => match serde_json::from_value(params) {
            Ok(request) => Ok(handler.request_permission(request).await.to_json()),
            Err(error) => Err((-32602, format!("Invalid permission request: {error}"))),
        },
        "fs/read_text_file" => match serde_json::from_value(params) {
            Ok(request) => handler
                .read_text_file(request)
                .await
                .map(|content| json!({ "content": content }))
                .map_err(|error| (-32603, error)),
            Err(error) => Err((-32602, format!("Invalid read request: {error}"))),
        },
        "fs/write_text_file" => match serde_json::from_value(params) {
            Ok(request) => handler
                .write_text_file(request)
                .await
                .map(|_| json!({}))
                .map_err(|error| (-32603, error)),
            Err(error) => Err((-32602, format!("Invalid write request: {error}"))),
        },
        other => Err((-32601, format!("Method '{other}' is not supported"))),
    };

    let response = match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    };

    if let Ok(mut encoded) = serde_json::to_string(&response) {
        encoded.push('\n');
        let mut writer = writer.lock().await;
        let _ = writer.write_all(encoded.as_bytes()).await;
        let _ = writer.flush().await;
    }
}

fn parse_session_notification(params: Value) -> Option<AcpSessionNotification> {
    let session_id = params.get("sessionId")?.as_str()?.to_string();
    let update = params.get("update")?.clone();
    Some(AcpSessionNotification {
        session_id,
        update: AcpSessionUpdate::from_value(update),
    })
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A connected agent with one open ACP session.
pub struct AcpSession {
    connection: Arc<AcpConnection>,
    session_id: String,
    agent: AcpInitializeResult,
    modes: Option<AcpSessionModeState>,
    /// Agent-defined settings, kept current as they are changed.
    config_options: Mutex<Vec<AcpConfigOption>>,
}

impl AcpSession {
    /// Spawns the agent, performs the handshake, and opens a session rooted at
    /// `cwd`.
    async fn start(
        config: &AcpAgentConfig,
        cwd: &Path,
        handler: Arc<dyn AcpClientHandler>,
    ) -> Result<Self, String> {
        let connection = Arc::new(AcpConnection::spawn(config, Some(cwd), handler).await?);
        let agent = match connection.initialize().await {
            Ok(agent) => agent,
            Err(error) => {
                connection.shutdown().await;
                return Err(error);
            }
        };
        let session = match connection.new_session(cwd, Vec::new()).await {
            Ok(session) => session,
            Err(error) => {
                connection.shutdown().await;
                return Err(error);
            }
        };
        Ok(Self {
            connection,
            session_id: session.session_id,
            agent,
            modes: session.modes,
            config_options: Mutex::new(session.config_options),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn agent(&self) -> &AcpInitializeResult {
        &self.agent
    }

    pub fn modes(&self) -> Option<&AcpSessionModeState> {
        self.modes.as_ref()
    }

    pub fn connection(&self) -> &Arc<AcpConnection> {
        &self.connection
    }

    /// Inspects the agent's session settings without cloning the vector.
    pub fn with_config_options<R>(&self, f: impl FnOnce(&[AcpConfigOption]) -> R) -> R {
        let guard = self.config_options.lock();
        let options = guard.as_deref().map(Vec::as_slice).unwrap_or(&[]);
        f(options)
    }

    /// Snapshot of the agent's session settings.
    pub fn config_options(&self) -> Vec<AcpConfigOption> {
        self.with_config_options(|options| options.to_vec())
    }

    /// Label the agent reports for `category`, such as which model it runs.
    pub fn config_label(&self, category: &str) -> Option<String> {
        self.with_config_options(|options| {
            config_option_for(options, category).and_then(AcpConfigOption::current_label)
        })
    }

    /// Sets one session setting, keeping the local snapshot in step with the
    /// agent's own answer.
    ///
    /// A value the agent does not offer is refused here rather than sent: an
    /// agent is free to reject it however it likes, including by failing the
    /// call, and a rejected setting must not look applied.
    pub async fn set_config_option(&self, category: &str, value: &str) -> Result<(), String> {
        let config_id = {
            let options = self.config_options();
            let Some(option) = config_option_for(&options, category) else {
                return Err(format!("This agent exposes no '{category}' setting"));
            };
            option.id.clone()
        };
        self.set_config_option_by_id(&config_id, value).await
    }

    /// Sets one session setting by its agent-assigned id.
    ///
    /// Ids are how an open-ended setting list is addressed: not every option
    /// carries a `category` (Claude Code's persona picker does not), so a
    /// category-only setter cannot reach all of them.
    ///
    /// A value the agent does not offer is refused here rather than sent: an
    /// agent is free to reject it however it likes, including by failing the
    /// call, and a rejected setting must not look applied.
    pub async fn set_config_option_by_id(
        &self,
        config_id: &str,
        value: &str,
    ) -> Result<(), String> {
        {
            let options = self.config_options();
            let Some(option) = options.iter().find(|option| option.id == config_id) else {
                return Err(format!("This agent exposes no '{config_id}' setting"));
            };
            if option.current_value() == Some(value) {
                return Ok(());
            }
            if !option.has_choice(value) {
                return Err(format!(
                    "This agent does not offer '{value}' for {}",
                    option.name
                ));
            }
        }
        let updated = self
            .connection
            .set_config_option(&self.session_id, config_id, value)
            .await?;
        if let Ok(mut options) = self.config_options.lock() {
            if !updated.is_empty() {
                *options = updated;
            } else if let Some(option) = options.iter_mut().find(|option| option.id == config_id) {
                option.current_value = Value::String(value.to_string());
            }
        }
        Ok(())
    }

    pub async fn prompt_text(&self, text: &str) -> Result<AcpStopReason, String> {
        self.connection
            .prompt(&self.session_id, vec![AcpContentBlock::text(text)])
            .await
    }

    pub async fn prompt(&self, blocks: Vec<AcpContentBlock>) -> Result<AcpStopReason, String> {
        self.connection.prompt(&self.session_id, blocks).await
    }

    pub async fn cancel(&self) -> Result<(), String> {
        self.connection.cancel(&self.session_id).await
    }

    pub async fn shutdown(&self) {
        self.connection.shutdown().await;
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpAgentStatus {
    Disconnected,
    Connecting,
    Connected {
        agent_name: String,
        protocol_version: u16,
    },
    Error(String),
}

impl AcpAgentStatus {
    pub fn display_status(&self) -> String {
        match self {
            Self::Disconnected => "Disconnected".to_string(),
            Self::Connecting => "Connecting...".to_string(),
            Self::Connected {
                agent_name,
                protocol_version,
            } => format!("Connected to {agent_name} (ACP v{protocol_version})"),
            Self::Error(error) => format!("Error: {error}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcpAgentRecord {
    pub config: AcpAgentConfig,
    pub status: AcpAgentStatus,
}

/// Discovers configured ACP agents and probes them for availability.
pub struct AcpManager {
    global_dir: Option<PathBuf>,
    project_root: Option<PathBuf>,
    agents: TokioMutex<Vec<AcpAgentRecord>>,
}

impl AcpManager {
    pub fn new(global_dir: Option<PathBuf>, project_root: Option<PathBuf>) -> Self {
        Self {
            global_dir,
            project_root,
            agents: TokioMutex::new(Vec::new()),
        }
    }

    /// Merges project and global configuration, with project entries winning on
    /// a shared id.
    pub fn configs(&self) -> Vec<AcpAgentConfig> {
        let global = AcpSettings::load_global(self.global_dir.as_deref());
        let project = AcpSettings::load_project(self.project_root.as_deref());

        Self::merge_configs(global, project)
    }

    fn merge_configs(
        global: Vec<AcpAgentConfig>,
        project: Vec<AcpAgentConfig>,
    ) -> Vec<AcpAgentConfig> {
        let mut merged = Vec::new();
        let mut seen = BTreeSet::new();
        for config in project.into_iter().chain(global) {
            if seen.insert(config.id.clone()) {
                merged.push(config);
            }
        }
        merged
    }

    /// Probes every enabled agent by completing an ACP handshake and then
    /// terminating the process. Disabled agents are reported without spawning.
    pub async fn discover_and_connect(&self) -> Vec<AcpAgentRecord> {
        let global_dir = self.global_dir.clone();
        let project_root = self.project_root.clone();
        let configs = tokio::task::spawn_blocking(move || {
            Self::merge_configs(
                AcpSettings::load_global(global_dir.as_deref()),
                AcpSettings::load_project(project_root.as_deref()),
            )
        })
        .await
        .unwrap_or_default();
        let mut records = Vec::new();
        for config in configs {
            let status = if config.enabled {
                Self::probe(&config, self.project_root.as_deref()).await
            } else {
                AcpAgentStatus::Disconnected
            };
            records.push(AcpAgentRecord { config, status });
        }

        *self.agents.lock().await = records.clone();
        records
    }

    pub async fn records(&self) -> Vec<AcpAgentRecord> {
        self.agents.lock().await.clone()
    }

    /// Completes a handshake and terminates the process.
    ///
    /// The probe runs with [`AcpProbeClient`], so an agent that issues
    /// filesystem or permission requests during `initialize` is refused rather
    /// than handed access to the current directory.
    async fn probe(config: &AcpAgentConfig, cwd: Option<&Path>) -> AcpAgentStatus {
        let handler: Arc<dyn AcpClientHandler> = Arc::new(AcpProbeClient);
        let mut connection = match AcpConnection::spawn(config, cwd, handler).await {
            Ok(connection) => connection,
            Err(error) => return AcpAgentStatus::Error(error),
        };
        let status = match connection.initialize().await {
            Ok(result) => AcpAgentStatus::Connected {
                agent_name: result.agent_display_name(),
                protocol_version: result.protocol_version,
            },
            Err(error) => {
                connection.shutdown().await;
                let stderr = match connection.stderr_task.take() {
                    Some(task) => Some(task.await.unwrap_or_default()),
                    None => None,
                };
                AcpAgentStatus::Error(Self::format_probe_error(error, stderr))
            }
        };
        connection.shutdown().await;
        status
    }

    fn format_probe_error(error: String, stderr: Option<Vec<u8>>) -> String {
        let stderr = stderr.and_then(|bytes| {
            let text = String::from_utf8_lossy(&bytes);
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (!text.is_empty()).then_some(text)
        });
        let detail = stderr
            .map(|text| format!("; stderr: {}", Self::truncate_status_text(&text, 180)))
            .unwrap_or_default();
        format!("{error}{detail}")
    }

    fn truncate_status_text(text: &str, max_chars: usize) -> String {
        let mut chars = text.chars();
        let truncated: String = chars.by_ref().take(max_chars).collect();
        if chars.next().is_some() {
            format!("{truncated}...")
        } else {
            truncated
        }
    }

    /// Opens a working session against the configured agent `id`.
    pub async fn start_session(
        &self,
        id: &str,
        cwd: &Path,
        handler: Arc<dyn AcpClientHandler>,
    ) -> Result<AcpSession, String> {
        let config = self
            .configs()
            .into_iter()
            .find(|config| config.id == id)
            .ok_or_else(|| format!("No ACP agent configured with id '{id}'"))?;
        if !config.enabled {
            return Err(format!("ACP agent '{}' is disabled", config.name));
        }
        AcpSession::start(&config, cwd, handler).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_config_round_trips_through_settings_file() {
        let config = AcpAgentConfig {
            id: "gemini".to_string(),
            name: "Gemini CLI".to_string(),
            command: "gemini".to_string(),
            args: vec!["--experimental-acp".to_string()],
            env: HashMap::new(),
            enabled: true,
            scope: AcpScope::Global,
        };
        let encoded = serde_json::to_string_pretty(&config).unwrap();
        assert!(encoded.contains("Gemini CLI"));
        let decoded: AcpAgentConfig = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn agent_config_defaults_enabled_and_global_scope() {
        let file: AcpSettingsFile = serde_json::from_str(
            r#"{ "agents": [{ "id": "claude", "name": "Claude Code", "command": "claude-code-acp" }] }"#,
        )
        .unwrap();
        assert_eq!(file.agents.len(), 1);
        assert!(file.agents[0].enabled);
        assert_eq!(file.agents[0].scope, AcpScope::Global);
        assert!(file.agents[0].args.is_empty());
    }

    #[test]
    fn from_command_line_splits_command_and_args() {
        let config = AcpAgentConfig::from_command_line(
            "Claude Code",
            "  npx -y claude-code-acp  ",
            AcpScope::Project,
        )
        .unwrap();
        assert_eq!(config.id, "claude_code");
        assert_eq!(config.command, "npx");
        assert_eq!(config.args, vec!["-y", "claude-code-acp"]);
        assert_eq!(config.scope, AcpScope::Project);
        assert_eq!(config.command_line(), "npx -y claude-code-acp");
    }

    #[test]
    fn from_command_line_rejects_blank_input() {
        assert!(AcpAgentConfig::from_command_line("", "gemini", AcpScope::Global).is_none());
        assert!(AcpAgentConfig::from_command_line("Gemini", "   ", AcpScope::Global).is_none());
    }

    #[test]
    fn probe_error_includes_normalized_truncated_stderr() {
        let error = AcpManager::format_probe_error(
            "ACP agent closed the connection".to_string(),
            Some(b" npm ERR!\n  package not found\n".to_vec()),
        );
        assert_eq!(
            error,
            "ACP agent closed the connection; stderr: npm ERR! package not found"
        );

        let error = AcpManager::format_probe_error("failed".to_string(), Some(vec![b'x'; 300]));
        assert!(error.ends_with("..."));
        assert!(error.len() < 220);
    }

    #[test]
    fn settings_save_and_load_round_trip_per_scope() {
        let dir = tempfile::tempdir().unwrap();
        let agents = vec![AcpAgentConfig {
            id: "gemini".to_string(),
            name: "Gemini".to_string(),
            command: "gemini".to_string(),
            args: vec!["--experimental-acp".to_string()],
            env: HashMap::new(),
            enabled: false,
            scope: AcpScope::Global,
        }];

        AcpSettings::save_global(dir.path(), &agents).unwrap();
        let loaded = AcpSettings::load_global(Some(dir.path()));
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].enabled);
        assert_eq!(loaded[0].scope, AcpScope::Global);

        AcpSettings::save_project(dir.path(), &agents).unwrap();
        let project = AcpSettings::load_project(Some(dir.path()));
        assert_eq!(project.len(), 1);
        // Scope is derived from the file the entry came from, not its contents.
        assert_eq!(project[0].scope, AcpScope::Project);
    }

    #[test]
    fn missing_settings_files_load_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(AcpSettings::load_global(Some(dir.path())).is_empty());
        assert!(AcpSettings::load_project(Some(dir.path())).is_empty());
        assert!(AcpSettings::load_global(None).is_empty());
        assert!(AcpSettings::load_project(None).is_empty());
    }

    #[test]
    fn project_agents_shadow_global_agents_with_the_same_id() {
        let global = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        AcpSettings::save_global(
            global.path(),
            &[
                AcpAgentConfig::from_command_line("Shared", "global-cmd", AcpScope::Global)
                    .unwrap(),
                AcpAgentConfig::from_command_line("Global Only", "other", AcpScope::Global)
                    .unwrap(),
            ],
        )
        .unwrap();
        AcpSettings::save_project(
            project.path(),
            &[
                AcpAgentConfig::from_command_line("Shared", "project-cmd", AcpScope::Project)
                    .unwrap(),
            ],
        )
        .unwrap();

        let manager = AcpManager::new(
            Some(global.path().to_path_buf()),
            Some(project.path().to_path_buf()),
        );
        let configs = manager.configs();
        assert_eq!(configs.len(), 2);
        let shared = configs.iter().find(|c| c.id == "shared").unwrap();
        assert_eq!(shared.command, "project-cmd");
        assert_eq!(shared.scope, AcpScope::Project);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discovery_loads_settings_off_async_worker() {
        let global = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let mut global_agent =
            AcpAgentConfig::from_command_line("Global", "global", AcpScope::Global).unwrap();
        global_agent.enabled = false;
        let mut project_agent =
            AcpAgentConfig::from_command_line("Project", "project", AcpScope::Project).unwrap();
        project_agent.enabled = false;
        AcpSettings::save_global(global.path(), &[global_agent]).unwrap();
        AcpSettings::save_project(project.path(), &[project_agent]).unwrap();
        let caller = std::thread::current().id();
        let manager = AcpManager::new(
            Some(global.path().to_path_buf()),
            Some(project.path().to_path_buf()),
        );

        let records = manager.discover_and_connect().await;

        assert_eq!(records.len(), 2);
        assert_ne!(last_settings_load_thread().unwrap(), caller);
    }

    #[test]
    fn session_update_decodes_known_variants() {
        let agent_chunk = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "hello" },
        }));
        assert_eq!(
            agent_chunk,
            AcpSessionUpdate::AgentMessageChunk(AcpContentBlock::text("hello"))
        );

        let thought = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": "thinking" },
        }));
        assert!(matches!(thought, AcpSessionUpdate::AgentThoughtChunk(_)));

        let tool_call = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "title": "Read main.rs",
            "kind": "read",
            "status": "pending",
        }));
        let AcpSessionUpdate::ToolCall(call) = tool_call else {
            panic!("expected a tool call update");
        };
        assert_eq!(call.tool_call_id, "call_1");
        assert_eq!(call.kind, Some(AcpToolKind::Read));
        assert_eq!(call.status, Some(AcpToolCallStatus::Pending));

        let plan = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "plan",
            "entries": [{ "content": "step", "priority": "high", "status": "pending" }],
        }));
        let AcpSessionUpdate::Plan(entries) = plan else {
            panic!("expected a plan update");
        };
        assert_eq!(entries.len(), 1);

        let mode = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "current_mode_update",
            "currentModeId": "ask",
        }));
        assert_eq!(
            mode,
            AcpSessionUpdate::CurrentModeUpdate {
                current_mode_id: "ask".to_string()
            }
        );
    }

    #[test]
    fn session_update_keeps_unknown_variants() {
        let update = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "usage_update",
            "tokens": 42,
        }));
        let AcpSessionUpdate::Other { kind, payload } = update else {
            panic!("expected an unknown update to be preserved");
        };
        assert_eq!(kind, "usage_update");
        assert_eq!(payload.get("tokens").and_then(Value::as_u64), Some(42));
    }

    #[test]
    fn unknown_content_block_type_does_not_fail() {
        let block: AcpContentBlock =
            serde_json::from_value(json!({ "type": "future_kind", "data": 1 })).unwrap();
        assert_eq!(block, AcpContentBlock::Unknown);
        assert!(block.as_text().is_none());
    }

    #[test]
    fn prompt_blocks_serialize_in_protocol_form() {
        let encoded = serde_json::to_value(vec![AcpContentBlock::text("hi")]).unwrap();
        assert_eq!(encoded, json!([{ "type": "text", "text": "hi" }]));
    }

    #[test]
    fn permission_policy_prefers_matching_option_kind() {
        let options = vec![
            AcpPermissionOption {
                option_id: "allow".to_string(),
                name: "Allow".to_string(),
                kind: AcpPermissionOptionKind::AllowOnce,
            },
            AcpPermissionOption {
                option_id: "always".to_string(),
                name: "Always allow".to_string(),
                kind: AcpPermissionOptionKind::AllowAlways,
            },
            AcpPermissionOption {
                option_id: "no".to_string(),
                name: "Reject".to_string(),
                kind: AcpPermissionOptionKind::RejectOnce,
            },
        ];

        assert_eq!(
            AcpPermissionPolicy::default().select(&options),
            AcpPermissionOutcome::Selected {
                option_id: "no".to_string()
            }
        );
        assert_eq!(
            AcpPermissionPolicy::AllowOnce.select(&options),
            AcpPermissionOutcome::Selected {
                option_id: "allow".to_string()
            }
        );
        assert_eq!(
            AcpPermissionPolicy::AllowAlways.select(&options),
            AcpPermissionOutcome::Selected {
                option_id: "always".to_string()
            }
        );
    }

    #[test]
    fn permission_policy_cancels_when_no_option_matches() {
        let options = vec![AcpPermissionOption {
            option_id: "weird".to_string(),
            name: "Weird".to_string(),
            kind: AcpPermissionOptionKind::Unknown,
        }];
        assert_eq!(
            AcpPermissionPolicy::AllowOnce.select(&options),
            AcpPermissionOutcome::Cancelled
        );
    }

    #[test]
    fn permission_outcome_serializes_to_protocol_shape() {
        assert_eq!(
            AcpPermissionOutcome::Selected {
                option_id: "allow".to_string()
            }
            .to_json(),
            json!({ "outcome": { "outcome": "selected", "optionId": "allow" } })
        );
        assert_eq!(
            AcpPermissionOutcome::Cancelled.to_json(),
            json!({ "outcome": { "outcome": "cancelled" } })
        );
    }

    #[test]
    fn stop_reason_decodes_known_and_unknown_values() {
        assert_eq!(
            AcpStopReason::from_value(&json!("end_turn")),
            AcpStopReason::EndTurn
        );
        assert_eq!(
            AcpStopReason::from_value(&json!("cancelled")),
            AcpStopReason::Cancelled
        );
        assert_eq!(
            AcpStopReason::from_value(&json!("something_new")),
            AcpStopReason::Unknown
        );
    }

    #[test]
    fn tool_call_tolerates_unknown_kind_and_status() {
        let call: AcpToolCall = serde_json::from_value(json!({
            "toolCallId": "call_1",
            "kind": "teleport",
            "status": "vibing",
        }))
        .unwrap();
        assert_eq!(call.tool_call_id, "call_1");
        assert_eq!(call.kind, None);
        assert_eq!(call.status, None);
    }

    #[test]
    fn permission_option_tolerates_unknown_kind() {
        let option: AcpPermissionOption = serde_json::from_value(json!({
            "optionId": "maybe",
            "name": "Maybe",
            "kind": "ask_later",
        }))
        .unwrap();
        assert_eq!(option.kind, AcpPermissionOptionKind::Unknown);
    }

    #[test]
    fn agent_status_display_reports_connection() {
        let connected = AcpAgentStatus::Connected {
            agent_name: "Gemini".to_string(),
            protocol_version: 1,
        };
        assert_eq!(connected.display_status(), "Connected to Gemini (ACP v1)");
        assert_eq!(
            AcpAgentStatus::Error("boom".to_string()).display_status(),
            "Error: boom"
        );
        assert_eq!(
            AcpAgentStatus::Disconnected.display_status(),
            "Disconnected"
        );
    }
}
