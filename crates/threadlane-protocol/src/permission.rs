use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    Once,
    Always,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub scope: Option<PermissionScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub session_id: Option<String>,
    pub capability: String,
    pub title: String,
    pub detail: String,
    #[serde(default)]
    pub scopes: Vec<PermissionScope>,
    #[serde(default)]
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow {
        scope: PermissionScope,
    },
    AllowOnce,
    AllowAlways,
    Deny,
    DenyWithReason {
        reason: String,
    },
    AllowWithModifications {
        scope: PermissionScope,
        modified_input: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitPermissionRequest {
    pub request_id: String,
    pub decision: PermissionDecision,
}
