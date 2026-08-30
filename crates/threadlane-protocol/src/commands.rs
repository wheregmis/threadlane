use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommandInfo {
    pub name: String,
    pub description: String,
    pub takes_argument: bool,
    pub argument_hint: Option<String>,
}

use std::path::Path;

pub fn available_slash_commands(_project_root: Option<&Path>) -> Vec<SlashCommandInfo> {
    vec![
        SlashCommandInfo {
            name: "task".to_string(),
            description: "Run a task in a background supervisor lane".to_string(),
            takes_argument: true,
            argument_hint: Some("<prompt>".to_string()),
        },
        SlashCommandInfo {
            name: "plan".to_string(),
            description: "Generate or update the session execution plan".to_string(),
            takes_argument: true,
            argument_hint: Some("<goal>".to_string()),
        },
        SlashCommandInfo {
            name: "compact".to_string(),
            description: "Compact session transcript history".to_string(),
            takes_argument: false,
            argument_hint: None,
        },
        SlashCommandInfo {
            name: "model".to_string(),
            description: "Switch the active LLM model".to_string(),
            takes_argument: true,
            argument_hint: Some("<model-id>".to_string()),
        },
        SlashCommandInfo {
            name: "skills".to_string(),
            description: "List discovered skills for this project".to_string(),
            takes_argument: false,
            argument_hint: None,
        },
    ]
}
