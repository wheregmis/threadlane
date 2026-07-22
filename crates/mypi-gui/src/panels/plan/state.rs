//! Plan panel state: task items, completion flags, and WASI extension state discovery.

use mypi_coding_agent::WasiExtensionManager;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct PlanItem {
    pub index: u64,
    pub description: String,
    pub completed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct PlanData {
    pub available: bool,
    pub enabled: bool,
    pub items: Vec<PlanItem>,
}

pub fn refresh_plan_data(data: &mut PlanData, work_dir: &Path, session_id: &str) -> bool {
    let state_path =
        WasiExtensionManager::session_state_path(work_dir, session_id, "plan_mode_ext");
    let state = std::fs::read_to_string(state_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok());

    let Some(state) = state else {
        data.available = false;
        data.enabled = false;
        data.items.clear();
        return false;
    };

    data.available = true;
    data.enabled = state
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    data.items.clear();
    if let Some(items) = state.get("items").and_then(serde_json::Value::as_array) {
        for item in items {
            if let (Some(index), Some(description)) = (
                item.get("index").and_then(serde_json::Value::as_u64),
                item.get("description").and_then(serde_json::Value::as_str),
            ) {
                data.items.push(PlanItem {
                    index,
                    description: description.to_string(),
                    completed: item
                        .get("completed")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                });
            }
        }
    }
    data.enabled
}
