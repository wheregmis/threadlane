use crate::agent::Agent;
use crate::plan_mode::PlanModeState;
use crate::session_tree::SessionTree;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    SwitchModel(String),
    Compact,
    ShowSession,
    SetName(String),
    SwitchTreeBranch(String),
    Fork(String),
    CloneSession,

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
        "compact" => Some(CommandAction::Compact),
        "session" => Some(CommandAction::ShowSession),
        "name" => Some(CommandAction::SetName(arg)),
        "tree" => Some(CommandAction::SwitchTreeBranch(arg)),
        "fork" => Some(CommandAction::Fork(arg)),
        "clone" => Some(CommandAction::CloneSession),

        "quit" | "exit" => Some(CommandAction::Quit),
        other => Some(CommandAction::Unknown(other.to_string())),
    }
}

pub async fn execute_slash_command(
    action: CommandAction,
    agent: &mut Agent,
    session_tree: &mut SessionTree,
    plan_mode: &mut PlanModeState,
) -> String {
    match action {
        CommandAction::SwitchModel(new_model) => {
            if new_model.is_empty() {
                let st = agent.get_state().await;
                format!("Current model: {}", st.model)
            } else {
                let mut st = agent.loop_engine.state.lock().await;
                st.model = new_model.clone();
                format!("Switched model to: {}", new_model)
            }
        }
        CommandAction::Compact => {
            agent.compact_history(None).await;
            "Context compacted successfully.".to_string()
        }
        CommandAction::ShowSession => {
            let st = agent.get_state().await;
            format!(
                "Session ID: {}\nName: {}\nMessage Count: {}\nModel: {}\nPlan Mode: {}",
                session_tree.session_id,
                session_tree.name.as_deref().unwrap_or("unnamed"),
                st.messages.len(),
                st.model,
                if plan_mode.enabled {
                    "Enabled 🟢"
                } else {
                    "Disabled ⚪"
                }
            )
        }
        CommandAction::SetName(name) => {
            session_tree.name = Some(name.clone());
            format!("Session name set to: {}", name)
        }
        CommandAction::SwitchTreeBranch(node_id) => {
            if session_tree.switch_active_node(&node_id) {
                let branch_msgs = session_tree.get_active_branch_messages();
                let mut st = agent.loop_engine.state.lock().await;
                st.messages = branch_msgs;
                format!("Switched session tree to node: {}", node_id)
            } else {
                format!("Node ID not found in session tree: {}", node_id)
            }
        }
        CommandAction::Fork(node_id) => {
            if let Some(forked) = session_tree.fork_branch(&node_id) {
                format!(
                    "Forked session tree successfully into ID: {}",
                    forked.session_id
                )
            } else {
                format!("Failed to fork. Node ID not found: {}", node_id)
            }
        }
        CommandAction::CloneSession => {
            let mut cloned = session_tree.clone();
            cloned.session_id = format!("{}_clone", session_tree.session_id);
            format!("Cloned active session tree into ID: {}", cloned.session_id)
        }

        CommandAction::Quit => "Quitting mypi agent.".to_string(),
        CommandAction::Unknown(cmd) => format!("Unknown command: /{}", cmd),
    }
}
