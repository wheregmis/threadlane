# `threadlane-agent`

`threadlane-agent` is the core agent execution engine responsible for session state trees, token compaction, and tool call dispatch loops.

## Features

- **Session History Tree**: Manages branching session histories, node parent-child relationships, durable JSONL persistence, and automatic session titling markers.
- **Context Compaction**: Truncates and compacts message histories when LLM context limits are approached, preserving system prompts and tool call dependencies.
- **Agent Execution Loop**: Manages turn iterations, dynamic tool registration/allowlisting, and hook interceptors (`before_tool_call`, `after_tool_call`).
