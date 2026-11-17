//! The system's tools and the milestone-1 permission policy (Plan.md §4.6,
//! §9.1, docs/milestone-1.md §9).

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use threadlane_core::model::{NewOutboxMessage, OutboxKind, Urgency};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{OutboxStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_core::tools::{
    GateRequest, PermissionPolicy, Tier, ToolContext, ToolDefinition, ToolError, ToolRegistry,
    Verdict,
};
use threadlane_store::HubDb;

use crate::error::tool_error;

/// The channel ordinary messages are queued on in milestone 1.
pub const CLIENT_CHANNEL: &str = "client";

/// Name of the lead's messaging tool.
pub const SEND_MESSAGE: &str = "send_message";

/// The key a call is recorded under when the backend supplied no call id:
/// `"<tool>:<sha256 of the canonical arguments>"` (Plan.md §4.6).
#[must_use]
pub fn derived_call_key(tool: &str, arguments: &Json) -> String {
    let mut canonical = String::new();
    write_canonical(arguments, &mut canonical);
    let digest = Sha256::digest(canonical.as_bytes());
    let mut key = format!("{tool}:");
    for byte in digest.iter() {
        // Writing to a String cannot fail.
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// JSON with object keys sorted and no insignificant whitespace, so equal
/// arguments hash equally whatever order the backend wrote them in.
fn write_canonical(value: &Json, out: &mut String) {
    match value {
        Json::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Json::String(key.clone()).to_string());
                out.push(':');
                if let Some(item) = map.get(key) {
                    write_canonical(item, out);
                }
            }
            out.push('}');
        }
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// The hub's built-in tools. Milestone 1 has one: `send_message`.
#[derive(Debug, Clone)]
pub struct HubTools {
    db: HubDb,
    clock: Arc<dyn Clock>,
    waker: Arc<dyn Waker>,
}

impl HubTools {
    /// Tools that write to `db` and wake the dispatcher through `waker`.
    #[must_use]
    pub fn new(db: HubDb, clock: Arc<dyn Clock>, waker: Arc<dyn Waker>) -> Self {
        Self { db, clock, waker }
    }

    fn send_message_definition() -> ToolDefinition {
        ToolDefinition {
            name: SEND_MESSAGE.to_owned(),
            description: "Send one message to the user. This is the only way anything you \
                          write reaches them. Send at most one per reply, and nothing when \
                          nothing needs saying."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The message, in plain language."
                    },
                    "urgency": {
                        "type": "string",
                        "enum": ["low", "normal", "high"],
                        "description": "How soon the user should see it. Defaults to normal."
                    }
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            tier: Tier::Local,
            safe_to_repeat: false,
        }
    }

    async fn send_message(
        &self,
        context: &ToolContext,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let (text, urgency) = parse_send_message(arguments)?;
        let run = self
            .db
            .run(context.run_id)
            .await
            .map_err(tool_error)?
            .ok_or_else(|| ToolError::Failed(format!("run {} not found", context.run_id)))?;
        // A backend-supplied id is stable across a crash and a resume of
        // the same session, so the session scopes it; a derived key only
        // identifies the call within its run.
        let key = match &context.call_id {
            Some(call_id) => format!("s{}:{call_id}", run.session_id),
            None => format!("r{}:{}", run.id, derived_call_key(SEND_MESSAGE, arguments)),
        };
        let message = self
            .db
            .enqueue(&NewOutboxMessage {
                project_id: Some(context.project_id),
                lane_id: Some(run.lane_id),
                session_id: Some(run.session_id),
                run_id: Some(run.id),
                tool_call_id: Some(key),
                kind: OutboxKind::Message,
                decision_id: None,
                channel: CLIENT_CHANNEL.to_owned(),
                body: json!({ "text": text }),
                urgency,
                not_before: None,
                created_at: self.clock.now(),
            })
            .await
            .map_err(tool_error)?;
        self.waker.wake(WakeTarget::Dispatcher);
        Ok(json!({ "outbox_id": message.id }))
    }
}

fn parse_send_message(arguments: &Json) -> Result<(String, Urgency), ToolError> {
    let invalid = |message: &str| ToolError::InvalidArguments(message.to_owned());
    let object: &Map<String, Json> = arguments
        .as_object()
        .ok_or_else(|| invalid("expected an object with a text field"))?;
    if let Some(unknown) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "text" | "urgency"))
    {
        return Err(ToolError::InvalidArguments(format!(
            "unknown field {unknown:?}"
        )));
    }
    let text = object
        .get("text")
        .and_then(Json::as_str)
        .ok_or_else(|| invalid("text must be a string"))?;
    if text.trim().is_empty() {
        return Err(invalid("text must not be empty"));
    }
    let urgency = match object.get("urgency") {
        None | Some(Json::Null) => Urgency::Normal,
        Some(Json::String(value)) => value
            .parse()
            .map_err(|_| invalid("urgency must be low, normal, or high"))?,
        Some(_) => return Err(invalid("urgency must be low, normal, or high")),
    };
    Ok((text.to_owned(), urgency))
}

#[async_trait]
impl ToolRegistry for HubTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![Self::send_message_definition()]
    }

    fn definition(&self, name: &str) -> Option<ToolDefinition> {
        (name == SEND_MESSAGE).then(Self::send_message_definition)
    }

    async fn execute(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        match name {
            SEND_MESSAGE => self.send_message(context, &arguments).await,
            other => Err(ToolError::UnknownTool(other.to_owned())),
        }
    }
}

/// Several registries as one. A name resolves to the first registry that
/// defines it, so a registry earlier in the list shadows later ones.
#[derive(Debug, Clone)]
pub struct CombinedTools {
    registries: Vec<Arc<dyn ToolRegistry>>,
}

impl CombinedTools {
    /// Combines `registries`, earlier ones first.
    #[must_use]
    pub fn new(registries: Vec<Arc<dyn ToolRegistry>>) -> Self {
        Self { registries }
    }

    fn owner(&self, name: &str) -> Option<&Arc<dyn ToolRegistry>> {
        self.registries
            .iter()
            .find(|registry| registry.definition(name).is_some())
    }
}

#[async_trait]
impl ToolRegistry for CombinedTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions: Vec<ToolDefinition> = Vec::new();
        for registry in &self.registries {
            for definition in registry.definitions() {
                if definitions.iter().all(|seen| seen.name != definition.name) {
                    definitions.push(definition);
                }
            }
        }
        definitions
    }

    fn definition(&self, name: &str) -> Option<ToolDefinition> {
        self.owner(name)
            .and_then(|registry| registry.definition(name))
    }

    async fn execute(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        match self.owner(name) {
            Some(registry) => registry.execute(context, name, arguments).await,
            None => Err(ToolError::UnknownTool(name.to_owned())),
        }
    }
}

/// Allows tools up to a tier and blocks the rest. Milestone 1 ships it
/// allowing tier 0 and 1 ([`TierPolicy::default`]); modes, standing rules,
/// and decisions replace it in milestone 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierPolicy {
    highest_allowed: Tier,
}

impl TierPolicy {
    /// A policy that allows every tier up to and including `highest_allowed`.
    #[must_use]
    pub fn allowing_up_to(highest_allowed: Tier) -> Self {
        Self { highest_allowed }
    }
}

impl Default for TierPolicy {
    fn default() -> Self {
        Self::allowing_up_to(Tier::Local)
    }
}

#[async_trait]
impl PermissionPolicy for TierPolicy {
    async fn decide(&self, request: &GateRequest) -> Verdict {
        if request.tier <= self.highest_allowed && request.tier != Tier::Forbidden {
            Verdict::Allow
        } else {
            Verdict::Block {
                reason: format!(
                    "{} is a {:?} tool, and this hub does not allow those yet",
                    request.tool, request.tier
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_keys_ignore_key_order_and_whitespace() {
        let a: Json = serde_json::from_str(r#"{"text": "hi", "urgency": "low"}"#).unwrap();
        let b: Json = serde_json::from_str(r#"{"urgency":"low","text":"hi"}"#).unwrap();
        assert_eq!(
            derived_call_key("send_message", &a),
            derived_call_key("send_message", &b)
        );
        assert_ne!(
            derived_call_key("send_message", &a),
            derived_call_key("send_message", &json!({"text": "hello"}))
        );
        let key = derived_call_key("send_message", &a);
        assert!(key.starts_with("send_message:"));
        assert_eq!(key.len(), "send_message:".len() + 64);
    }

    #[test]
    fn send_message_arguments_are_checked() {
        assert_eq!(
            parse_send_message(&json!({"text": "hi"})).unwrap(),
            ("hi".to_owned(), Urgency::Normal)
        );
        assert_eq!(
            parse_send_message(&json!({"text": "hi", "urgency": "high"})).unwrap(),
            ("hi".to_owned(), Urgency::High)
        );
        for bad in [
            json!("hi"),
            json!({}),
            json!({"text": "  "}),
            json!({"text": "hi", "urgency": "now"}),
            json!({"text": "hi", "to": "everyone"}),
        ] {
            assert!(
                matches!(
                    parse_send_message(&bad),
                    Err(ToolError::InvalidArguments(_))
                ),
                "{bad} is rejected"
            );
        }
    }

    #[tokio::test]
    async fn the_milestone_one_policy_allows_tiers_zero_and_one() {
        let policy = TierPolicy::default();
        let request = |tier| GateRequest {
            project_id: threadlane_core::ids::ProjectId(1),
            agent_id: None,
            tool: "t".into(),
            tier,
            arguments: json!({}),
            binding_mode: threadlane_core::model::Mode::OnRequest,
            tainted: false,
        };
        assert_eq!(policy.decide(&request(Tier::Read)).await, Verdict::Allow);
        assert_eq!(policy.decide(&request(Tier::Local)).await, Verdict::Allow);
        for tier in [
            Tier::SharedReversible,
            Tier::SharedHardToUndo,
            Tier::Forbidden,
        ] {
            assert!(matches!(
                policy.decide(&request(tier)).await,
                Verdict::Block { .. }
            ));
        }
    }
}
