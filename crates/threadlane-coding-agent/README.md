# `threadlane-coding-agent`

`threadlane-coding-agent` provides the high-level coding agent harness built on top of `threadlane-agent`, `threadlane-tools`, and `threadlane-provider`.

## Key Capabilities

- **WASI Capability Broker**: Runs WebAssembly extensions via Wasmi. Extensions import `threadlane_host.request` for sandboxed access to workspace tools, subagents, and session state.
- **Skill & Preset Discovery**: Loads project-local and global skill definitions (`.threadlane/skills/`, `.agents/skills/`), custom prompts (`.threadlane/prompts/`), and subagent presets (`.threadlane/agents/`).
- **Subagent Manager**: Spawns and manages concurrent background subagents (`scout`, `worker`) with progress event streams.
- **System Prompt Builder**: Dynamically builds structured system prompts incorporating available tools, discovered skills, project layout, and prompt templates.
