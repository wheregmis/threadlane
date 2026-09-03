use crate::capabilities_catalog::CapabilityCatalog;
use std::path::Path;
use threadlane_runtime::AgentRuntime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandInfo {
    pub name: String,
    pub description: String,
}

/// Built-in slash commands handled by the coding agent.
pub fn builtin_commands() -> Vec<SlashCommandInfo> {
    [
        ("model", "Switch model, or show the current one"),
        (
            "prewalk",
            "Explore and land first working edit, then hand off to fast model (/prewalk <objective>)",
        ),
        ("compact", "Compact the conversation context"),
        ("session", "Show session info"),
        ("name", "Name this session"),
        ("skill", "Load a discovered skill by ID"),
        (
            "subagent",
            "Delegate tasks to subagents in parallel or sequentially",
        ),
        ("task", "Run a prompt as a background task"),
        ("quit", "Quit threadlane agent"),
    ]
    .into_iter()
    .map(|(name, description)| SlashCommandInfo {
        name: name.to_string(),
        description: description.to_string(),
    })
    .collect()
}

/// All slash commands available to the user, including built-ins and
/// commands contributed by active extensions.
pub fn available_slash_commands(project_root: Option<&Path>) -> Vec<SlashCommandInfo> {
    let mut commands = builtin_commands();
    let catalog = CapabilityCatalog::discover(project_root);
    for record in catalog.extensions() {
        if !record.is_effective() || !record.is_enabled() {
            continue;
        }
        if let Ok(ext) =
            threadlane_wasi::WasiExtension::load_from_file_requiring_manifest(record.module_path())
        {
            for cmd in ext.manifest.commands {
                if !commands.iter().any(|c| c.name == cmd.name) {
                    commands.push(SlashCommandInfo {
                        name: cmd.name,
                        description: cmd.description,
                    });
                }
            }
        }
    }
    commands
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    SwitchModel(String),
    Prewalk(String),
    Compact,
    ShowSession,
    SetName(String),
    InvokeSkill(String),
    PromptTemplate(String),
    Subagent(String),
    Quit,
    Unknown(String),
}

pub fn parse_slash_command(input: &str) -> Option<CommandAction> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let mut parts = trimmed[1..].split_whitespace();
    let cmd = parts.next()?;
    let arg = parts.collect::<Vec<&str>>().join(" ");

    match cmd {
        "model" => Some(CommandAction::SwitchModel(arg)),
        "prewalk" => Some(CommandAction::Prewalk(arg)),
        "compact" => Some(CommandAction::Compact),
        "session" => Some(CommandAction::ShowSession),
        "name" => Some(CommandAction::SetName(arg)),
        "skill" => Some(CommandAction::InvokeSkill(arg)),
        "prompt" => Some(CommandAction::PromptTemplate(arg)),
        "subagent" => Some(CommandAction::Subagent(arg)),

        "quit" | "exit" => Some(CommandAction::Quit),
        other => Some(CommandAction::Unknown(other.to_string())),
    }
}

pub async fn execute_slash_command(action: CommandAction, agent: &mut AgentRuntime) -> String {
    match action {
        CommandAction::SwitchModel(new_model) => {
            if new_model.is_empty() {
                format!("Current model: {}", agent.model())
            } else {
                let _ = agent
                    .harness_mut()
                    .set_fact("main", "model", new_model.clone(), None);
                let _ = agent.drive_harness();
                {
                    let mut st = agent.turn.lock().await;
                    st.model = new_model.clone();
                }
                format!("Switched model to: {}", new_model)
            }
        }
        CommandAction::Prewalk(objective) => {
            if objective.trim().is_empty() {
                "Usage: /prewalk <task objective> to explore, land the first edit, and transition to fast model.".to_string()
            } else {
                format!("Prewalk initiated for: {}", objective.trim())
            }
        }
        CommandAction::Compact => {
            if !agent.compact_history(None).await {
                "Nothing to compact yet.".to_string()
            } else {
                "Context compacted in the current session.".to_string()
            }
        }
        CommandAction::ShowSession => {
            let st = agent.get_state().await;
            format!(
                "Session ID: {}\nMessage Count: {}\nModel: {}",
                agent.session_id,
                st.messages.len(),
                st.model,
            )
        }
        CommandAction::SetName(name) => {
            let _ = agent
                .harness_mut()
                .set_fact("main", "name", name.clone(), None);
            let _ = agent.drive_harness();
            format!("Session name set to: {}", name)
        }
        CommandAction::InvokeSkill(skill) => format!("Invoking skill: {}", skill),
        CommandAction::PromptTemplate(tmpl) => format!("Prompt template: {}", tmpl),
        CommandAction::Subagent(task) => {
            let trimmed = task.trim();
            if trimmed.is_empty() {
                "Usage: /subagent <task description>".to_string()
            } else {
                format!("Delegating subagent task: {trimmed}")
            }
        }

        CommandAction::Quit => "Quitting threadlane agent.".to_string(),
        CommandAction::Unknown(cmd) => format!("Unknown command: /{}", cmd),
    }
}
