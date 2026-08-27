use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpScope {
    Global,
    Project,
}

impl AcpScope {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Project => "Project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpAgentStatus {
    Connecting,
    Connected,
    Disconnected,
    Failed,
}

impl AcpAgentStatus {
    pub fn display_status(&self) -> &'static str {
        match self {
            Self::Connecting => "Connecting…",
            Self::Connected => "Connected",
            Self::Disconnected => "Disconnected",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAgentConfig {
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
    pub scope: AcpScope,
    pub enabled: bool,
}

impl AcpAgentConfig {
    pub fn from_command_line(name: &str, command: &str, scope: AcpScope) -> Option<Self> {
        let parts: Vec<String> = command.split_whitespace().map(|s| s.to_string()).collect();
        if parts.is_empty() {
            return None;
        }
        let id = name
            .to_lowercase()
            .replace(' ', "_")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        Some(Self {
            id,
            name: name.to_string(),
            command: parts,
            scope,
            enabled: true,
        })
    }

    pub fn command_line(&self) -> String {
        self.command.join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAgentRecord {
    pub config: AcpAgentConfig,
    pub status: AcpAgentStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionScope {
    Global,
    Project,
}

impl ExtensionScope {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Project => "Project",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRecord {
    pub id: String,
    pub name: String,
    pub version: String,
    pub scope: ExtensionScope,
    pub module_path: PathBuf,
    pub enabled: bool,
}

impl ExtensionRecord {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn scope(&self) -> ExtensionScope {
        self.scope
    }
    pub fn module_path(&self) -> &PathBuf {
        &self.module_path
    }
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
    pub fn is_effective(&self) -> bool {
        self.enabled
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillScope {
    Global,
    Project,
}

impl SkillScope {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Project => "Project",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub scope: SkillScope,
    #[serde(default)]
    pub is_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub supports_reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAgentDescriptor {
    pub id: String,
    pub name: String,
    pub command: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListModelsResponse {
    pub models: Vec<ModelDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSkillsRequest {
    pub project_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSkillsResponse {
    pub skills: Vec<SkillDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToggleSkillRequest {
    pub project_path: String,
    pub skill_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfoResponse {
    pub version: String,
    pub protocol_version: String,
    pub os: String,
    pub arch: String,
    pub workspace_count: usize,
}

// ── ACP Agent CRUD ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAcpAgentsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAcpAgentsResponse {
    pub agents: Vec<AcpAgentRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddAcpAgentRequest {
    pub name: String,
    pub command: String,
    pub scope: AcpScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetAcpEnabledRequest {
    pub id: String,
    pub enabled: bool,
    pub scope: AcpScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveAcpAgentRequest {
    pub id: String,
    pub scope: AcpScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
}

// ── Provider Auth ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Antigravity,
    OpenAi,
    OpenCode,
    GitHub,
    GitLab,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAuthStatusResponse {
    pub provider: ProviderKind,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// True while a browser-based authentication flow is waiting for completion.
    #[serde(default)]
    pub pending: bool,
    /// A sanitized error from the most recent browser-based authentication flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetProviderAuthRequest {
    pub provider: ProviderKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectProviderRequest {
    pub provider: ProviderKind,
    /// For providers requiring an API key (OpenAI, OpenCode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectProviderResponse {
    pub status: String,
    /// For OAuth providers: the browser URL GPUI should open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisconnectProviderRequest {
    pub provider: ProviderKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexAccountRecord {
    pub id: String,
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCodexAccountsResponse {
    pub accounts: Vec<CodexAccountRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetActiveCodexAccountRequest {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveCodexAccountRequest {
    pub id: String,
}

// ── Git extended operations ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStageFileRequest {
    pub project_path: String,
    pub file_path: String,
    /// true = stage, false = unstage
    pub stage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitRequest {
    pub project_path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitResponse {
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPushPullRequest {
    pub project_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiscardFileRequest {
    pub project_path: String,
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitIgnoreRequest {
    pub project_path: String,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitMergeRequest {
    pub project_path: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStashActionRequest {
    pub project_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitDiffMessageResponse {
    pub message: String,
}

// ── Session title generation ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateTitleRequest {
    pub session_id: String,
    pub project_path: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateTitleResponse {
    pub title: String,
}
