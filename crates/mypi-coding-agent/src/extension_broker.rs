use async_trait::async_trait;
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

#[async_trait]
pub trait CapabilityHandler: Send + Sync {
    fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError>;

    fn handle_for_extension(
        &self,
        request: &BrokerRequest,
        _invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        self.handle(request)
    }

    async fn handle_for_extension_async(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        self.handle_for_extension(request, invoking_extension)
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

#[derive(Debug, Clone, PartialEq)]
pub struct BrokerOperationResult {
    pub invoking_extension: String,
    pub request: BrokerRequest,
    pub value: Value,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrokerDispatchResult {
    /// Successful operation outputs are delivered to the extension as queued
    /// events, never folded into the importing invocation's result.
    pub operation_results: Vec<BrokerOperationResult>,
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
                .cloned()
                .ok_or_else(|| BrokerError {
                    code: "unknown_capability".into(),
                    message: format!(
                        "Host does not implement capability `{}`",
                        request.capability
                    ),
                })?;
            let value = handler
                .handle_for_extension_async(request, &envelope.invoking_extension)
                .await?;
            result.operation_results.push(BrokerOperationResult {
                invoking_extension: envelope.invoking_extension.clone(),
                request: request.clone(),
                value,
            });
        }
        Ok(result)
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
            format!("Capability `{capability}` is not granted to this extension"),
        )
    }
}

/// Host-owned grant restrictions applied in addition to a v2 manifest's
/// declaration. The default preserves compatibility by granting declarations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostCapabilityGrantPolicy {
    allowed: Option<BTreeSet<String>>,
}

impl HostCapabilityGrantPolicy {
    pub fn allow_declared() -> Self {
        Self::default()
    }

    pub fn restrict_to(granted: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allowed: Some(granted.into_iter().map(Into::into).collect()),
        }
    }

    pub fn allows_declared(&self, declared: &[String], capability: &str) -> bool {
        declared.iter().any(|item| item == capability)
            && self
                .allowed
                .as_ref()
                .is_none_or(|allowed| allowed.contains(capability))
    }
}
