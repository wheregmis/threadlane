//! Command palette state: slash commands definition and matching.

#[derive(Clone, Debug)]
pub struct CommandInfo {
    /// Command name without the leading slash.
    pub name: String,
    pub description: String,
}

/// Built-in slash commands (kept in sync with mypi-agent/src/commands.rs).
pub fn builtin_commands() -> Vec<CommandInfo> {
    [
        ("model", "Switch model, or show the current one"),
        ("compact", "Compact the conversation context"),
        ("session", "Show session info"),
        ("name", "Name this session"),
        ("tree", "Switch session tree branch"),
        ("fork", "Fork a session tree branch"),
        ("clone", "Clone the active session tree"),
        ("clear-plan", "Clear active plan items"),
        ("quit", "Quit mypi agent"),
    ]
    .into_iter()
    .map(|(name, description)| CommandInfo {
        name: name.to_string(),
        description: description.to_string(),
    })
    .collect()
}
