use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

pub const BROKER_API_VERSION: u32 = 2;

/// A host-side envelope. Extensions can create `BrokerRequest`, but only the
/// host attaches the identity used by identity-sensitive capabilities.
#[derive(Debug, Clone)]
pub struct HostBrokerRequest {
    pub request: BrokerRequest,
    pub invoking_extension: String,
}

pub trait CapabilityHandler: Send + Sync {
    fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError>;

    fn handle_for_extension(
        &self,
        request: &BrokerRequest,
        _invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        self.handle(request)
    }
}

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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrokerDispatchResult {
    pub message: Option<String>,
    pub follow_up_prompt: Option<String>,
}

#[derive(Default)]
pub struct CapabilityDispatcher {
    handlers: HashMap<String, Arc<dyn CapabilityHandler>>,
}

impl CapabilityDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, capability: impl Into<String>, handler: Arc<dyn CapabilityHandler>) {
        self.handlers.insert(capability.into(), handler);
    }

    pub async fn dispatch(
        &mut self,
        requests: Vec<BrokerRequest>,
    ) -> Result<BrokerDispatchResult, BrokerError> {
        self.dispatch_envelopes(
            requests
                .into_iter()
                .map(|request| HostBrokerRequest {
                    request,
                    invoking_extension: String::new(),
                })
                .collect(),
        )
        .await
    }

    pub async fn dispatch_envelopes(
        &mut self,
        requests: Vec<HostBrokerRequest>,
    ) -> Result<BrokerDispatchResult, BrokerError> {
        let mut result = BrokerDispatchResult::default();
        for envelope in requests {
            let request = &envelope.request;
            if request.api_version != BROKER_API_VERSION {
                return Err(BrokerError {
                    code: "invalid_request".into(),
                    message: format!("Unsupported broker API version: {}", request.api_version),
                });
            }
            let handler = self
                .handlers
                .get(&request.capability)
                .ok_or_else(|| BrokerError {
                    code: "unknown_capability".into(),
                    message: format!(
                        "Host does not implement capability `{}`",
                        request.capability
                    ),
                })?;
            let value = handler.handle_for_extension(request, &envelope.invoking_extension)?;
            append_outputs(&mut result, &value);
        }
        Ok(result)
    }
}

fn append_outputs(result: &mut BrokerDispatchResult, value: &Value) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in ["message", "ui_notification"] {
        if let Some(message) = object.get(key).and_then(Value::as_str) {
            append_text(&mut result.message, message);
        }
    }
    for key in ["follow_up_prompt", "queued_agent_prompt"] {
        if let Some(prompt) = object.get(key).and_then(Value::as_str) {
            append_text(&mut result.follow_up_prompt, prompt);
        }
    }
}

fn append_text(target: &mut Option<String>, text: &str) {
    if text.is_empty() {
        return;
    }
    match target {
        Some(existing) => {
            existing.push('\n');
            existing.push_str(text);
        }
        None => *target = Some(text.to_string()),
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
