# `threadlane-provider`

`threadlane-provider` implements OpenAI-compatible REST and Codex backend clients with streaming SSE and WebSocket support.

## Key Modules

- **`OpenAIClient`**: Handles streaming chat completions, reasoning tokens, tool calls, and title generation requests.
- **`Auth`**: Provides device authorization code flow helpers, OAuth token loading, and credential persistence under `~/.threadlane/auth.json`.
- **Cache Key Clamping**: Performs Unicode-safe prompt cache key generation and clamping for LLM provider caching headers.
