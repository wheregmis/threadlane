# Subagent Extension for `mypi` Harness

The `subagent-ext` dynamic extension enables task delegation to specialized AI subagents with isolated context windows, customized model/tool settings, and support for single, parallel, and sequential chain workflows.

## Structure

```
extensions/subagent_ext/
├── Cargo.toml          # Extension crate definition
├── src/
│   └── lib.rs          # Extension entry point & tool registration
├── agents/             # Agent presets
│   ├── scout.md        # Fast recon
│   ├── planner.md      # Implementation planning
│   ├── reviewer.md     # Code audit
│   └── worker.md       # Full execution worker
└── prompts/            # Workflow presets
    ├── implement.md            # scout -> planner -> worker
    ├── scout-and-plan.md       # scout -> planner
    └── implement-and-review.md # worker -> reviewer -> worker
```

## Modes

1. **Single**: `{ "agent": "scout", "task": "find auth code" }`
2. **Parallel**: `{ "tasks": [{ "agent": "scout", "task": "..." }, ...] }`
3. **Chain**: `{ "chain": [{ "agent": "scout", "task": "..." }, { "agent": "planner", "task": "plan based on {previous}" }] }`
