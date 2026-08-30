use std::sync::Arc;
use serde::{Deserialize, Serialize};

// ── Update release info ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReleaseInfo {
    pub version: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

// ── Update status ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    Available(UpdateReleaseInfo),
    UpToDate,
    Downloading {
        version: String,
        progress: f32,
    },
    ReadyToInstall {
        info: UpdateReleaseInfo,
        #[serde(skip)]
        bytes: Arc<Vec<u8>>,
    },
    Installing,
    Error(String),
}

// ── Requests ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckForUpdateResponse {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadUpdateRequest {
    pub version: String,
    pub url: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallUpdateRequest {
    pub version: String,
}

/// Pushed as a `update/progress` notification during download.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateProgressEvent {
    pub version: String,
    pub progress: f32,
    pub done: bool,
    pub error: Option<String>,
}
