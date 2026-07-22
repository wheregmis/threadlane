use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const BROKER_API_VERSION: u32 = 2;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BrokerRequest {
    pub api_version: u32,
    pub capability: String,
    pub operation: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BrokerError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BrokerResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BrokerError>,
}

impl BrokerResponse {
    pub fn ok(value: Value) -> Self {
        Self {
            ok: true,
            value: Some(value),
            error: None,
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            value: None,
            error: Some(BrokerError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CapabilityPolicy {
    granted: BTreeSet<String>,
}

impl CapabilityPolicy {
    pub fn new(granted: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            granted: granted.into_iter().map(Into::into).collect(),
        }
    }

    pub fn allows(&self, capability: &str) -> bool {
        self.granted.contains(capability)
    }

    pub fn denied_response(&self, capability: &str) -> BrokerResponse {
        BrokerResponse::error(
            "capability_denied",
            format!("Extension did not declare capability `{capability}`"),
        )
    }
}
