use super::{
    persist_json_state, read_json_state, validate_extension_id, BrokerError, BrokerReceipt,
    BrokerRequest, StateWriteError, WasiExtensionEvent, BROKER_API_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, sync::Arc};

const FORMAT: &str = "threadlane.extension-checkpoint";

/// Host-owned terminal output, committed with extension state before delivery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedToolReply {
    pub identity: threadlane_protocol::ToolExecutionIdentity,
    pub extension_name: String,
    pub tool_name: String,
    pub arguments: Value,
    pub(crate) result: Result<String, String>,
    pub(crate) broker_receipt_ids: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_result: Option<threadlane_protocol::AgentToolResult>,
}

impl SavedToolReply {
    pub(crate) fn result(
        &self,
        events: &[Arc<WasiExtensionEvent>],
    ) -> Result<Result<String, String>, String> {
        let mut outcomes = Vec::with_capacity(self.broker_receipt_ids.len());
        for id in &self.broker_receipt_ids {
            let event = events.iter().find(|event| event.topic == "broker_response" && event.payload["receipt_id"].as_u64() == Some(*id))
                .ok_or_else(|| format!("Saved reply for tool {} is waiting for broker receipt {id}; reconcile the original outcome, do not execute the tool or broker operation again", self.identity.tool_call_id))?;
            outcomes.push(event.as_ref());
        }
        for event in &outcomes {
            if event.payload["ok"] == false {
                return Ok(Err(event.payload["error"]["message"]
                    .as_str()
                    .unwrap_or("Broker operation failed")
                    .into()));
            }
        }
        if let Err(error) = &self.result {
            return Ok(Err(error.clone()));
        }
        let message = outcomes
            .iter()
            .find(|event| {
                event.payload["capability"] == "agent" && event.payload["operation"] == "run"
            })
            .or_else(|| outcomes.last())
            .and_then(|event| {
                event.payload["value"]["message"]
                    .as_str()
                    .or_else(|| event.payload["value"]["output"].as_str())
            });
        Ok(Ok(message
            .map(str::to_owned)
            .unwrap_or_else(|| self.result.as_ref().unwrap().clone())))
    }
}

#[derive(Serialize, Deserialize, PartialEq)]
pub(crate) struct BrokerIntent {
    pub receipt: BrokerReceipt,
    pub request: BrokerRequest,
}

impl BrokerIntent {
    pub fn matches_event(&self, event: &WasiExtensionEvent) -> bool {
        event.topic == "broker_response"
            && event.payload["receipt_id"].as_u64() == Some(self.receipt.id)
            && event.payload["api_version"].as_u64() == Some(self.request.api_version as u64)
            && event.payload["capability"].as_str() == Some(self.request.capability.as_str())
            && event.payload["operation"].as_str() == Some(self.request.operation.as_str())
            && event.payload["arguments"] == self.request.arguments
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionCheckpoint {
    #[serde(rename = "$threadlane")]
    format: String,
    version: u32,
    pub state: Value,
    pub broker_events: Vec<WasiExtensionEvent>,
    #[serde(default)]
    pub last_broker_id: u64,
    #[serde(default)]
    pub unsettled: Vec<BrokerIntent>,
    #[serde(default)]
    pub terminal_reply: Option<SavedToolReply>,
}

impl ExtensionCheckpoint {
    pub fn decode(value: Value) -> Result<Self, String> {
        if value.get("$threadlane").and_then(Value::as_str) != Some(FORMAT) {
            return Ok(Self {
                format: FORMAT.into(),
                version: 1,
                state: value,
                broker_events: vec![],
                last_broker_id: 0,
                unsettled: vec![],
                terminal_reply: None,
            });
        }
        let checkpoint: Self = serde_json::from_value(value)
            .map_err(|error| format!("Invalid extension checkpoint: {error}"))?;
        if !matches!(checkpoint.version, 1 | 2 | 3) || checkpoint.format != FORMAT {
            return Err("Unsupported extension checkpoint version; preserve the file".into());
        }
        let mut ids = std::collections::HashSet::new();
        if (checkpoint.version == 1
            && (!checkpoint.unsettled.is_empty() || checkpoint.last_broker_id != 0))
            || checkpoint.unsettled.iter().any(|intent| {
                intent.receipt.id == 0
                    || intent.receipt.id > checkpoint.last_broker_id
                    || intent.request.api_version != BROKER_API_VERSION
                    || !ids.insert(intent.receipt.id)
            })
        {
            return Err("Invalid extension checkpoint broker intent; preserve the file".into());
        }
        if checkpoint
            .broker_events
            .iter()
            .any(|event| !valid_broker_event(event))
        {
            return Err("Invalid extension checkpoint broker event; preserve the file".into());
        }
        let mut event_ids = std::collections::HashSet::new();
        for event in &checkpoint.broker_events {
            if let Some(value) = event.payload.get("receipt_id") {
                let id = value
                    .as_u64()
                    .filter(|id| *id != 0 && *id <= checkpoint.last_broker_id)
                    .ok_or("Invalid checkpoint outcome receipt; preserve the file")?;
                if !event_ids.insert(id)
                    || checkpoint
                        .unsettled
                        .iter()
                        .any(|intent| intent.receipt.id == id && !intent.matches_event(event))
                {
                    return Err(
                        "Duplicate or mismatched checkpoint outcome receipt; preserve the file"
                            .into(),
                    );
                }
            }
        }
        if let Some(reply) = &checkpoint.terminal_reply {
            let mut ids = std::collections::HashSet::new();
            if checkpoint.version < 3
                || !reply
                    .identity
                    .matches_call(&reply.identity.tool_call_id, &reply.identity.tool_name)
                || validate_extension_id(&reply.extension_name).is_err()
                || reply.tool_name.trim().is_empty()
                || (reply.result.is_err() && !reply.broker_receipt_ids.is_empty())
                || reply
                    .broker_receipt_ids
                    .iter()
                    .any(|id| *id == 0 || *id > checkpoint.last_broker_id || !ids.insert(*id))
                || reply.canonical_result.as_ref().is_some_and(|result| {
                    !reply
                        .identity
                        .matches_call(&result.tool_call_id, &result.name)
                })
                || (reply.canonical_result.is_some()
                    && (!checkpoint.unsettled.is_empty()
                        || reply.broker_receipt_ids.iter().any(|id| {
                            !checkpoint
                                .broker_events
                                .iter()
                                .any(|event| event.payload["receipt_id"].as_u64() == Some(*id))
                        })))
            {
                return Err("Invalid extension terminal reply; preserve the checkpoint".into());
            }
        }
        Ok(checkpoint)
    }
}

fn valid_broker_event(event: &WasiExtensionEvent) -> bool {
    let payload = &event.payload;
    if event.topic != "broker_response"
        || payload["api_version"].as_u64() != Some(BROKER_API_VERSION as u64)
        || !payload["capability"].is_string()
        || !payload["operation"].is_string()
        || payload.get("arguments").is_none()
    {
        return false;
    }
    match payload["ok"].as_bool() {
        Some(true) => payload.get("value").is_some() && payload.get("error").is_none(),
        Some(false) => {
            payload.get("value").is_none()
                && payload.get("error").is_some_and(|error| {
                    serde_json::from_value::<BrokerError>(error.clone()).is_ok()
                })
        }
        None => false,
    }
}

/// Disabled and removed modules retain checkpoints. Their receipt numbers
/// still belong to this scope, even when inventory loading never visits them.
pub(crate) fn visit_checkpoints(
    directory: &Path,
    mut visit: impl FnMut(&Path, ExtensionCheckpoint) -> Result<(), String>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(directory).map_err(|error| {
        format!(
            "Cannot inspect receipt checkpoints {}: {error}",
            directory.display()
        )
    })? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_name = entry.file_name();
        let Some(name) = file_name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        // Only extension state files own receipt IDs. Host state, the owner
        // lock, and staging files have separate filename namespaces.
        if validate_extension_id(name).is_err() && !name.starts_with(".encoded-") {
            continue;
        }
        let path = entry.path();
        let value = read_json_state(&path)?.ok_or_else(|| format!("Receipt checkpoint {} vanished during recovery; inspect storage before allocating new receipts", path.display()))?;
        let checkpoint = ExtensionCheckpoint::decode(value)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        visit(&path, checkpoint)?;
    }
    Ok(())
}

pub(crate) fn receipt_high_water(directory: &Path) -> Result<u64, String> {
    let mut high_water = 0;
    visit_checkpoints(directory, |_, checkpoint| {
        high_water = high_water.max(checkpoint.last_broker_id);
        Ok(())
    })?;
    Ok(high_water)
}

pub(crate) fn persist_checkpoint(
    path: &Path,
    state: &Value,
    events: &[Arc<WasiExtensionEvent>],
    last_broker_id: u64,
    unsettled: &[Arc<BrokerIntent>],
    terminal_reply: Option<&SavedToolReply>,
) -> Result<(), StateWriteError> {
    #[derive(Serialize)]
    struct Checkpoint<'a> {
        #[serde(rename = "$threadlane")]
        format: &'static str,
        version: u32,
        state: &'a Value,
        broker_events: Vec<&'a WasiExtensionEvent>,
        last_broker_id: u64,
        unsettled: Vec<&'a BrokerIntent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        terminal_reply: Option<&'a SavedToolReply>,
    }
    persist_json_state(
        path,
        &Checkpoint {
            format: FORMAT,
            version: if terminal_reply.is_some() { 3 } else { 2 },
            state,
            broker_events: events
                .iter()
                .filter(|event| event.topic == "broker_response")
                .map(Arc::as_ref)
                .collect(),
            last_broker_id,
            unsettled: unsettled.iter().map(Arc::as_ref).collect(),
            terminal_reply,
        },
    )
}
