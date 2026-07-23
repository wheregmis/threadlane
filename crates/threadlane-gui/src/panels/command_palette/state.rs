//! Command palette state: slash commands definition and matching.

#[derive(Clone, Debug)]
pub struct CommandInfo {
    /// Command name without the leading slash.
    pub name: String,
    pub description: String,
}

/// Built-in slash commands handled by the coding agent.
pub fn builtin_commands() -> Vec<CommandInfo> {
    [
        ("model", "Switch model, or show the current one"),
        ("compact", "Compact the conversation context"),
        ("session", "Show session info"),
        ("name", "Name this session"),
        ("tree", "Switch session tree branch"),
        ("fork", "Fork a session tree branch"),
        ("clone", "Clone the active session tree"),
        ("skill", "Load a discovered skill by ID"),
        ("clear-plan", "Clear active plan items"),
        ("quit", "Quit threadlane agent"),
    ]
    .into_iter()
    .map(|(name, description)| CommandInfo {
        name: name.to_string(),
        description: description.to_string(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_command_is_discoverable() {
        let commands = builtin_commands();

        assert!(commands.iter().any(|command| command.name == "skill"));
    }
}
