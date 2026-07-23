# Subagent Extension for `mypi` Harness

The `subagent-ext` WASI extension exposes task delegation both as the `/subagent` slash command and as a model-callable `subagent` tool. Both entry points normalize to the v2 broker operation `agent.run`, using isolated subagent context windows and supporting single, parallel, and sequential workflows.

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

## Model-callable tool

The manifest declares a `subagent` function tool with one canonical argument shape:

```json
{
  "tasks": [
    { "agent": "scout", "task": "find auth code" },
    { "agent": "planner", "task": "plan based on {previous}" }
  ],
  "parallel": false
}
```

`tasks` must be a non-empty array of objects containing non-empty `agent` and `task` strings. Set `parallel` to `true` for independent concurrent tasks or `false` for array-order execution. During sequential execution, the host replaces `{previous}` with the prior task result.

## Slash command forms

The slash command retains its concise and compatibility aliases:

1. **Plain text (single scout)**: `/subagent find auth code`
2. **Single JSON task**: `/subagent {"agent":"reviewer","task":"review auth"}` (`agent` defaults to `scout`)
3. **Parallel tasks**: `/subagent {"tasks":[{"agent":"scout","task":"..."},{"agent":"reviewer","task":"..."}]}`
4. **Canonical tasks**: add `"parallel": false` or `true` explicitly to a `tasks` object
5. **Sequential aliases**: use either `"chain"` or `"sequential"`; later task prompts may include `{previous}`

Example chain:

```json
{
  "sequential": [
    { "agent": "scout", "task": "find auth code" },
    { "agent": "planner", "task": "plan based on {previous}" }
  ]
}
```

Malformed structured input and empty task lists return the existing usage-style response and do not issue a broker request.
