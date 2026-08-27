# AGENTS.md

Guidance for coding agents working in the Threadlane repository.

## Scope

This file applies to the entire repository.

Threadlane is a Rust workspace centered on a native GPUI desktop application (`crates/threadlane-gpui`). Keep changes focused, preserve the existing visual language, and prefer established project patterns over introducing new frameworks or dependencies.

## Repository Map

- `crates/threadlane-gpui/` — GPUI native desktop application and primary binary.
  - `src/` — application state, GPUI views, panels, terminal integration, and composer state.
- `crates/threadlane-runtime/` — agent execution engine, harness V2 durability, provider routing, compaction, and turn driver.
- `crates/threadlane-session/` — session orchestration (CodingAgent, supervisor), ACP client, WASI broker, skills, subagents, and project context.
- `crates/threadlane-provider/` — model/provider and authentication integrations.
- `crates/threadlane-tools/` — tool implementations and capability support.
- `crates/threadlane-auth/` — authentication helpers.
- `crates/threadlane-git/` — git repository integration.
- `crates/threadlane-updater/` — signed update checks, downloads, installation, and relaunch.
- `crates/threadlane-wasi/` — WASI extension runner.
- `extensions/` — WASI extensions built for `wasm32-wasip1`.
- `scripts/build_extensions.sh` — builds and deploys bundled extensions, agents, and prompts into `.threadlane/`.
- `packaging/`, `.github/workflows/`, and package metadata — release and platform packaging.

Do not edit generated content under `target/` or deployed runtime content under `.threadlane/` unless the task explicitly concerns generated artifacts.

## Common Commands

Run commands from the repository root.

```bash
# Fast validation for desktop-app changes
cargo check -p threadlane-gpui

# Focused updater tests
cargo test -p threadlane-updater

# Full workspace tests
cargo test --workspace

# Build and deploy WASI extensions
./scripts/build_extensions.sh

# Run the GPUI desktop app
cargo run -p threadlane-gpui

# Check patch whitespace
git diff --check
```

For a local updater UI check against the published manifest:

```bash
THREADLANE_UPDATER_PUBLIC_KEY="$(cat threadlane-updater.key.pub)" \
cargo run -p threadlane-gpui
```

A normal `cargo run` may be unsuitable for testing installation: update installation and relaunch are intentionally restricted to a packaged `.app`.

## Validation Expectations

1. Start with the narrowest relevant test or check.
2. For Rust or GPUI edits, run at least:
   - `cargo check -p threadlane-gpui`
   - `git diff --check`
3. Run focused tests for touched logic, then broader workspace tests when warranted.
4. Do not claim a UI behavior was visually verified unless the application was actually run and observed.
5. Existing unused-code warnings are not part of unrelated tasks; do not remove meaningful code merely to silence them.
6. The locked GPUI/Zed revision uses `std::hint::cold_path`, which requires Rust 1.95 or newer. Keep the root `rust-toolchain.toml` and release workflow aligned with that minimum.

## Rust and Architecture Conventions
- **Strict reuse gate:** Before implementing anything, search the repository for an existing component, helper, state type, command path, or dependency that already provides the needed behavior. Reuse or extend the existing implementation whenever possible. Do not create a duplicate component, abstraction, utility, or parallel state path unless you document why the existing one cannot satisfy the requirement.
- Prefer `edit_file_hashline` for high-precision edits using line:hash anchors (e.g. '12:a3f') returned from `read_file` to ensure edit safety and prevent line drift. Use range edits (`start_anchor` to `end_anchor`) for multi-line replacements/deletions, batch multiple edits into a single tool call, and re-read the target range with `read_file` if a hash mismatch occurs.

- Keep edits surgical. Do not move unrelated symbols or reformat large files without need.
- Use `edit_files_hashline` when logically coupled changes span multiple files: it validates every workspace path, anchor, and overlap before committing any target. LSP semantic mutation tools return non-mutating workspace-edit plans; convert and apply those plans through the host anchored transaction rather than allowing an extension to write source files directly.
- Reuse existing dependencies and runtime infrastructure.
- Preserve separation between reusable components, panel-specific behavior, shared state, and the top-level app shell.
- Keep chat behavior and session/sidebar behavior well-structured within GPUI views.
- Prefer root-cause fixes over state-specific offsets or visual patches.
- Avoid holding locks across expensive work, UI callbacks, or async boundaries.
- Preserve user work and persisted session data. Never casually delete `.threadlane` state or session files.

## Session and Context-Menu Behavior

- Project terminal groups are keyed by canonical project work directory, not by session ID. Each project can own multiple independent shell tabs; switching sessions in one project must retain its shells, active tab, and output, while switching projects selects that project's terminal group.
- The GPUI terminal is a persistent `portable-pty` shell parsed through `vt100`, not a command-by-command `sh -lc` console. Keep PTY reads off the UI thread, apply parser updates through GPUI entity updates, forward focused keyboard input directly to the PTY, and retain terminal entities by project when switching workspaces.
- Subagent dispatch creates and commits a dedicated child harness lane before provider execution. Execute the child with the `AcceptedRun` for that existing lane; never call foreground prompt acceptance on `main`, append a duplicate child prompt, or treat an agent role name as a lane selector.
- Durable coding sessions prepare the current model-visible context at every provider-attempt boundary, including tool-loop attempts. This bounded request context is distinct from cumulative processed usage, which may exceed the model limit across a long session. Regression coverage must drive real tool execution and durable usage through the production runtime/harness path, prove every emitted request stays below its recorded effective context limit, and prove required compaction commits its branch and telemetry before the next `ProviderRequestStarted` while the original transcript and durable marker survive reload; never rewrite only `TurnDriver` state or derive current-context UI from cumulative usage.
- Model-managed todo plans are session-scoped and persisted as complete `session_plan` records in the existing session JSONL. Show only the active session's plan above the project-wide task groups; do not derive plan state from compactable tool-call history or merge it into supervisor task state.

- The project attach button appears while hovering the `PROJECTS` header.
- The canonical attached-project registry is owned by `threadlane-session` at `~/.threadlane/projects.json`; GPUI must use that API rather than maintaining a second registry. On first load, merge the legacy `~/.threadlane/gui/projects.json` metadata into the canonical records and remove the legacy file only after an atomic canonical save succeeds. Preserve supervisor task metadata and newer GUI project/session selections when rebasing stale writers.
- The context-target state is distinct from the active-session state.
- The sidebar project filter is presentation-only and distinct from both active-session and context-target state. It filters the existing flat session list by owning attached project; selecting a filter must not switch sessions, change the composer project, or persist project selection.
- Archive and delete actions should flow through `SessionContextMenuAction` and the app’s existing action handler.
- A worktree session can have a project-level `.threadlane/sessions/<id>.jsonl` metadata stub while its full durable transcript lives at `<worktree>/.threadlane/sessions/<id>.jsonl`. Both lightweight startup discovery and full background discovery must prefer the existing worktree-local transcript, while retaining the stub as the fallback before that transcript exists; they must produce the same session-file key so hydration is not discarded as stale, and must never replace visible history with the metadata-only file. Spawn startup hydration explicitly when constructing `WorkspaceView`; do not rely on the later workspace event pump receiving a wake event before the initial transcript load begins. If the recorded worktree directory is absent, keep the durable session visible but label it as not checked out in both the sidebar and active composer; PR lookup must continue through the canonical project and recorded branch so merged status remains visible without a local checkout.

## Model Provider Routing

- Provider selection is encoded in the persisted model ID. Models prefixed with `antigravity/` or `opencode-go/` route through `threadlane-provider::router::ProviderClient`; unprefixed models retain the OpenAI path. Preserve the prefix across model switching, sessions, subagents, and payload construction.
- Persist each session's selected model in `SessionTree` metadata. Restore it before constructing the agent runtime and synchronize the model picker from that restored value; legacy metadata without a model continues to use the caller-provided default.
- A restored session has two synchronized representations: the persisted `SessionTree` active branch and `AgentState.messages`, which supplies provider context. Every constructor or session-switch path must load the active branch into `AgentState.messages` after the current system prompt; populating only the chat UI makes old messages visible without sending them to the model and also breaks subsequent prefix-based persistence. Independently, GPUI must hydrate its visible transcript from the canonical session JSONL on startup and every session selection; do not render a runtime/provider-context snapshot as chat history, because it can omit persisted reasoning, responses, or tool activity.
- Keep the central agent loop provider-neutral. Provider clients must translate requests and stream results into the shared `StreamEvent`, `ToolCall`, and `ProviderUsage` contract so tool execution, hooks, compaction, persistence, and chat rendering are not duplicated.
- OpenAI Responses events distinguish streaming `*.delta` events from final `*.done` snapshots. Emit only explicit text/reasoning deltas; never pass `response.*.done` fields through generic text fallbacks, or final output is duplicated and reasoning snapshots can leak into assistant content.
- OpenAI device login uses Codex's registered public OAuth client ID and the PKCE data returned by `/api/accounts/deviceauth/token`. Keep the client ID synchronized with the current Codex login implementation, preserve `code_verifier`, exchange the authorization code as `application/x-www-form-urlencoded`, and use `https://auth.openai.com/deviceauth/callback` as the redirect URI; stale client IDs or the legacy `/device` exchange fail after browser authorization.
- Antigravity uses Google Cloud Code Assist's `v1internal` endpoints and outer request envelope, not the public Gemini `streamGenerateContent` endpoint. Preserve project discovery, production/daily endpoint fallback, runtime-model mapping, wrapped SSE parsing, and provider-specific tool schemas when changing that client.
- Gemini tool calls can include a required `thoughtSignature`. Preserve it on the shared persisted `ToolCall` and replay it on the assistant `functionCall` part; dropping it causes the next tool-result request to fail with HTTP 400.
- Credential checks follow the selected model. Antigravity models require stored Antigravity OAuth credentials but must not require an OpenAI key; OpenAI models retain the existing OpenAI credential requirement.
- Automatic session titles must route through `ProviderClient` so provider-prefixed models use their own credentials and request format. OpenCode titles use streamed Chat Completions even when an OpenAI ChatGPT/Codex account is also configured; skip the title side path for Antigravity sessions rather than consuming an OpenAI credential or permanently marking a failed Antigravity title attempt.

## Background Tasks and Capabilities

- `HarnessSupervisor` owns only explicit background tasks (currently `/task <prompt>`). Ordinary chat sessions continue to use the existing `SessionRuntime` path and must not be mirrored into supervisor tasks. Supervisor task status is derived from canonical harness snapshots and events; it does not own a second operation log or lane-recovery authority.
- `CodingSessionHarness` (`coding_agent/harness.rs`) is the canonical session adapter. Production code must route foreground, `/task`, and model subagent durable operations through it. No production caller may directly append session or operation-log records outside this harness path.
- Durable operations are intent-first: persist `OperationStarted`, `StepAttempt`, `ToolStarted`, and `QueueEnqueued` through `CodingSessionHarness`/`SessionAgent` before starting provider/tool work. `ToolExecutionStart` is observational only.
- Every provider-backed run persists a non-model-visible `RunContextCaptured` snapshot plus low-volume provider, permission, physical-tool, cancellation, subagent, and bounded stream-checkpoint records in the canonical JSONL. Never serialize credentials, headers, cookies, OAuth data, unrestricted provider payloads, or raw provider bodies. System prompts are captured deliberately with SHA-256 and a 256 KiB cap; set `THREADLANE_REDACT_SYSTEM_PROMPTS=1` to persist only the digest, byte length, and redaction reason.
- Child intent is durable before model/tool work; checkpoints use `WriteDeferred`; safe replay is automatic and unsafe interruption aborts. Child subagent lanes use the canonical `SessionAgent` path with deterministic identity derived from parent session + tool-call ID. A subagent source/parent must be a canonical persisted entry ID; never retain an in-memory compatibility `node_N` as durable lineage, and clear the source identity when lane start falls back to no parent.
- Concurrent child lanes can hold independently opened JSONL stores. Reload and rebase stale sequence inputs while holding the shared writer gate at the append boundary; sequence numbers allocated from an earlier snapshot are not authoritative.
- GPUI model mutations must call `cx.notify()` inside the `Entity::update` callback when observers need to redraw; mutating `AppState` without notification leaves optimistic messages and streamed updates invisible until another interaction causes a render.
- Keep long chat transcripts and trajectory event lists on GPUI's variable-height `ListState`. Load chat JSONL backward in bounded turn-aligned pages and use `ListState::splice` when prepending older rows so its logical scroll anchor advances with the insertion. Trajectory caches hold lightweight row descriptors and aggregate statistics; render only visible event rows and format raw JSON only for the selected inspector entry.
- For remaining `ScrollHandle` views, `offset().y` becomes negative as content scrolls down while `max_offset().y` is positive. Compute distance from the bottom as `abs(offset.y + max_offset.y)`, and notify the owning view after setting a deferred scroll target so prepaint applies it against the latest layout.
- In the GPUI chat path, `CodingAgent` is the sole owner of durable prompt persistence. Show an accepted prompt optimistically in `AppState.messages`, but do not also append it directly to `SessionTree` before `handle_input_with_images`, which would persist the same user message twice. Forward `AgentEvent`s to the GPUI thread and reconcile from the session file only when the run finishes.
- GPUI `Task` is `#[must_use]` and **dropping it cancels the future immediately**. `let _ = cx.spawn(...)` therefore never runs. Either `.detach()` the task or hold/`await` it; the async spawn runs the work to completion on the background executor.
- GPUI sessions reuse one `SessionRuntime`/`CodingAgent` per durable session on a shared Tokio executor. Register each spawned turn with `CodingAgentCancellation::track_active_run` before exposing cancellation, and call `finish_active_run` after normal completion; `cancel()` records durable abort intent before aborting the registered task.
- GPUI message queue and steer actions must use the active runtime's `CodingAgentWorkHandle` (`try_queue_follow_up_with_images` and `queue_steer_with_images`). Stage composer text per session, persist queue intent before optimistic presentation, and let `CodingAgent::run_scheduled_agent_work` consume it rather than creating a GUI-only queue.
- `CodingAgent::new` opens `CodingSessionHarness` against the supplied session JSONL itself. Harness V2 records and legacy session records intentionally coexist in that canonical file; do not invent a GPUI-only `.harness.jsonl` sidecar. Existing `.harness.jsonl` filtering remains for legacy sidecars.
- Metadata-only session updates (name, model, and automatic-title attempt state) append a latest-wins `session_metadata` record instead of replacing the whole canonical JSONL. Whole-file replacement can race harness appends and discard operation records or entries.
- After a provider turn, persist newly produced provider-state messages through `CodingSessionHarness::sync_messages` before asserting model-visible durability. Reasoning is represented as a model-visible `Custom { custom_type: "thinking" }` message and must be logged before `assert_model_visible` runs. Model-visible durability is exact ordered equality, not a subsequence check; extra durable messages are also divergence.
- Passive branch commits must reload under the canonical session writer gate and append only the rebased nodes/metadata. Never transactionally replace canonical JSONL from a stale `SessionTree`, because concurrent harness records can be discarded.
- GPUI trajectory completion reconciles from canonical JSONL. Use transcript entry sequence for tool chronology and lifecycle records only to enrich run/lane identity; tool call IDs are not globally unique. Do not project ephemeral provider `TurnStart`/`TurnEnd` events or durable outer `StepAttempt` records as equivalent trajectory turns.
- Reducer lane leaves advance monotonically by entry sequence; replaying an older `StepAttempt` must not move the leaf behind later reasoning, tool results, or assistant entries. Multi-tool results form a source-ordered parent chain so every result remains on the model-visible branch; recovery may temporarily attach a synthesized result to the assistant when an earlier safe result is still pending replay.
- Threadlane extensions are compiled WASI modules with an exported `extension_info` manifest. The settings picker installs a `.wasm` into either `~/.threadlane/extensions/` or `<project>/.threadlane/extensions/`; it never runs Cargo or extension build scripts. Native extension executables and trust approvals are unsupported. LSP remains a WASI extension and launches language servers through brokered process capability.

### Project-Scoped Skill Enable/Disable

- Skills are toggled per project, not globally. `SkillSettings` persists disabled skill IDs in `<project>/.threadlane/skills.json`; skill discovery (`Discovery::finish`) applies those overrides so a disabled skill stays visible in the settings list with `enabled: false` but is excluded from the model catalog and rejected by `load_skill`.
- A toggle must clear `capability_cache`, refresh the capabilities chip / slash commands via `refresh_project_capabilities`, and call `refresh_live_session_skills` so running sessions re-discover skills. `CodingAgent::refresh_skills` swaps the shared `SkillRegistry` `Arc`; note the already-registered `LoadSkillToolExecutor` holds the previous `Arc`, so an in-flight session keeps the catalog from its creation and a fresh session fully reflects the toggle.

## External ACP Agents

- Threadlane is an Agent Client Protocol *client*: it launches a third-party agent as a subprocess and speaks newline-delimited JSON-RPC 2.0 over its stdio pipes. It is not an ACP agent server, and ACP has no non-stdio transport, so an `AcpAgentConfig` is always a spawnable command.
- `crates/threadlane-session/src/acp.rs` owns the protocol. Follow the `mcp.rs` precedent rather than adding a protocol SDK dependency: the wire format is hand-rolled with `serde_json` over `tokio::process`, which keeps the runtime model consistent with the rest of the workspace.
- Configuration mirrors MCP: `acp.json` in the global Threadlane directory and in `<project>/.threadlane/`. Project entries shadow global entries with the same `id`, unparsable or oversized files load as empty, and the scope on a loaded config always comes from the file it was read from, never from the file's contents.
- ACP grows by adding enum variants. Decode defensively: unknown `session/update` kinds become `AcpSessionUpdate::Other`, unknown content blocks become `AcpContentBlock::Unknown`, and unknown tool kinds, tool statuses, permission kinds, and stop reasons degrade to `None`/`Unknown` instead of failing the surrounding message. A newer agent must never break an in-flight turn.
- `AcpConnection` is bidirectional. The reader task resolves pending client requests by id and dispatches agent-initiated requests (`fs/read_text_file`, `fs/write_text_file`, `session/request_permission`) to the `AcpClientHandler`; every unimplemented method must answer `-32601` rather than going silent, or the agent blocks forever.
- `session/update` notifications are dispatched inline on the connection's read loop so they keep the order the agent emitted them; streamed chunks and tool-call updates are meaningless reordered. Only agent-initiated *requests* get a spawned task, because those can block on a user decision. An `AcpClientHandler::on_session_update` implementation therefore must hand the update off rather than block.
- `session/prompt` has no client-side timeout: a turn runs until the agent reports a stop reason, and `session/cancel` is the interrupt. Reserve timeouts for the handshake and other bounded calls.
- Probing an agent grants it nothing. `AcpManager::probe` runs with `AcpProbeClient`, which refuses every filesystem method and cancels every permission request, so checking whether an unproven third-party binary launches never hands it access to the current directory. Do not swap in a workspace-backed handler to make a probe "more realistic".
- Agent-driven filesystem access is workspace-scoped through `threadlane_tools::validate_path_in_workspace`. Do not add a second path-guard implementation, and do not widen the guard to satisfy an agent that asks for absolute paths outside the project.
- That guard resolves a not-yet-existing target by joining the remaining components onto its canonicalized nearest existing ancestor, then comparing against the canonical root. Never compare a lexical path against the canonical root: a workspace reached through a symlink is spelled two ways (`/tmp/...` and `/private/tmp/...` on macOS), so the lexical check rejects valid new files anywhere under it.
- The default `AcpPermissionPolicy` is `Reject`. An unattended client has no informed consent to give, so auto-approval must stay opt-in and any UI-backed handler should prompt rather than raise this default.
- Build connections through `AcpConnection::from_streams` when testing. `tests/acp_tests.rs` pairs the client with an in-process stub agent over `tokio::io::duplex`, which covers framing, request correlation, and the sandbox without depending on an installed agent binary.
- An ACP agent is selected as a model id of the form `acp/<agent_id>`, reusing the `antigravity/` prefix convention so it flows through the existing picker, `/model`, and per-session model persistence.
- ACP session updates are mapped onto `AgentEvent` in `acp_bridge`, not rendered through a parallel path. Keep that mapping pure and in the session crate so it stays testable.
- Stopping an ACP turn must send `session/cancel` as well as aborting the task. Aborting only stops Threadlane listening; the external agent keeps working.
- `acp_runtime::AcpEngine` is the only thing that runs an ACP turn, and `CodingAgent` holds one for the life of the session. Reuse is the point: an ACP agent owns its own conversation, so opening a session per turn would silently discard the agent's context. Switching agents shuts the old one down first rather than leaking the subprocess.
- ACP turns are dispatched before the harness run in `handle_input_with_images`, not through it. `begin_harness_run` journals a provider, a resolved tool schema, and a replayable message list, none of which an ACP agent has; recording one as an OpenAI run with Threadlane's tools would make the trajectory lie. The consequence is that ACP turns are not yet journaled — fix that by giving the harness an ACP-shaped run, not by forcing the existing shape.
- A permission request from an agent goes to the user through `PermissionHandle::request_external`, which renders the same `AgentEvent::PermissionRequested` prompt as the native tools. Answers are mapped back onto option ids the agent actually sent; a decision with no matching option cancels rather than substituting a near-miss, because silently upgrading a denial to an allow is the worst failure available here.
- ACP models carry no Threadlane provider credential. Anything gating a turn on `provider_credentials` being non-empty must exempt `is_acp_model`, or every ACP turn fails before it starts.
- An agent exposes its own settings as `configOptions` on `session/new`, changed with `session/set_config_option` (`configId`, not `configOptionId`). This is how Threadlane learns which model an agent is running and how it applies the reasoning picker — there are no protocol fields for either. Match options by `category` (`model`, `thought_level`), never by position or by `id`, which is agent-defined. Effort is re-applied on every turn because the picker can change between turns.
- Only the agent's *model* setting is surfaced in the UI, inside the composer's model picker. The rest of `configOptions` (permission mode, persona) is read and settable through `AcpEngine` but deliberately has no control yet. Set options by `id`, not `category`: Claude Code's persona option carries no category at all.
- Never send a config value the agent did not advertise. Claude Code silently coerces an unknown model id onto its nearest known alias (`claude-opus-5` becomes `opus[1m]`) instead of erroring, so a blind write reports success while running a different model. The advertised option list is the only safe source of values.
- The agent's model choices are rendered in the composer's model picker, not the settings control: selecting the ACP agent and selecting the model it runs are both "which model am I on", and splitting them across two controls made picking a model look like it did nothing.
- A settings change is applied against the agent and the *agent's* reply is stored, never an optimistic local value: changing one option can change another (picking a model changes which effort levels exist), and a value the agent does not offer is refused before it is sent.
- The reasoning picker sets `AgentRuntime` config, which the ACP path never reads; anything that must reach an external agent has to go through `set_config_option`. The same trap applies to any future setting that looks provider-shaped.
- An ACP model label comes from the agent's option *description*, not its name: agents label the current choice "Default (recommended)", which does not say what is running, and put "Opus 4.8 with 1M context" in the description.
- `tests/acp_engine_tests.rs` drives the real spawn path against `src/bin/acp_stub_agent.rs`, behind the `test-support` feature: `cargo test -p threadlane-session --features test-support`. Cover new turn behaviour there rather than only in unit tests — the handshake, ordering, permission, and cancel paths only exist once a process is on the other end. Keep the engine alive outside a task the test aborts, mirroring the app: the connection is `kill_on_drop`, so dropping it with the turn kills the agent before it can act on the cancel.

## Performance

- PR Hotpath harnesses live in the non-published `crates/threadlane-benchmarks` crate, with one binary per independently reported suite. Keep measurement executables and their shared fixtures there rather than adding benchmark examples to production crates; production entry points should expose only the narrow APIs needed by real callers and benchmarks.
- Measure before changing. `crates/threadlane-mcp/tests/perf_baseline.rs` and `crates/threadlane-runtime/tests/perf_baseline.rs` are `#[ignore]`d measurement harnesses, not assertions; run them with `-- --ignored --nocapture` to get a baseline and again to prove a change helped. Do not optimize a path whose cost has not been measured.
- For UI debugging and profiling, build `target/debug/threadlane-gpui` and drive that exact binary with Computer Use; never assume `/Applications/Threadlane.app` contains the current changes. When Computer Use needs an app identity, copy the binary into a temporary `.app` with a distinct bundle ID such as `dev.threadlane.sourceprofile`, ad-hoc re-sign it after every binary replacement, verify the source and bundled binary hashes match, and run `codesign --verify --deep --strict` before launch.
- Target the temporary bundle ID in Computer Use, confirm the installed `dev.threadlane.app` is not running, and resolve the exact-source PID before attaching Instruments. Prefer accessibility elements; when GPUI controls lack labels, inspect a fresh screenshot and use top-level `x`/`y` coordinates. Use `THREADLANE_GPUI_PROFILE=1` for the frame overlay and Time Profiler for stack attribution, and do not claim visual verification until the exact-source window was observed.
- Beware first-exec cost when benchmarking spawned processes. The first execution of a freshly written script costs ~200ms on macOS (a one-time system check), which lands inside whatever you are timing and reads as a product problem. `perf_baseline.rs` warms the stub up first; MCP discovery is ~5.5ms, not the ~200ms an unwarmed harness reports.
- Pin a performance fix with a *behavioral* test, not a timing one. `tests/session_reuse.rs` counts how many server processes actually start, so it fails for the right reason on a loaded CI machine.
- MCP servers are long-lived: `McpManager` keeps one `McpSession` per server id and reuses it across tool calls. Do not reintroduce spawn-per-call — it cost ~5 ms per call against a trivial shell stub and far more against a real `npx`-based server. A failed exchange retires the session so the next call reconnects, and `Command::kill_on_drop` cleans up when the manager drops.
- Each MCP session carries its own lock. Hold the session map only long enough to look up or install a handle, never across the request round trip, or tool calls to unrelated servers serialize behind each other.
- Session files are parsed once through the untagged `SessionLine` enum. Do not go back to trying `SessionRecord` and then `SessionNode`, which parsed the JSON text of every node line twice.
- JSONL durability appends are incremental: `JsonlStore` owns a `ReductionContext` and each append runs a guard (pure validation) then a commit against live state — no history clone, no post-append full reduce. `Reducer::reduce` uses that cached projection for JSONL/`AgentHarness<JsonlStore>` and rebuilds only for stores without incremental state or when JSONL is loaded/reloaded. Do not reintroduce `validate_candidate_*`-style clone-and-fully-reduce validation into `JsonlStore`; memory/sqlite stores still use those helpers. The safety invariant — live context projects identically to a fresh reduction at every step, including after reload — is pinned by `incremental_appends_match_full_reduction` in jsonl.rs; extend that test when adding record variants with new cross-record rules.
- `GatedEffects` are inert until `drive_to_completion`; production `CodingSessionHarness` uses its owned `JsonlStore` directly and must drive accepted intents before starting provider/tool work. Do not add a second executor-owned JSONL store or restore unconditional post-drive refreshes, which reparses the canonical session after every record.
- Streaming-reduce build order matters: id uniqueness phases run before sequence checks, entry streaming applies leaves before parent/lane validation over the complete set (forward parent references are legal), the preferred-leaf override sits between the entry and record streams, and runtime guards validate the *previously committed* preferred leaf while commits observe the updated one. Changing that order changes which error a corrupt file reports.

## Release Automation

- Use Conventional Commit subjects for commits and squash-merge titles: `<type>[optional scope][optional !]: <description>`. Use a type configured in `release-please-config.json` (`feat`, `fix`, `perf`, `refactor`, `build`, `ci`, `chore`, or `revert`) so Release Please can parse the change.
- Keep the `release` job in `.github/workflows/release-please.yml` serialized per branch with stale-run cancellation. A release-please run can observe a release PR merged after its triggering push; without job-level concurrency, that stale run can create the new version tag on its older commit.
- When a package starts inheriting `workspace.package.version`, add its `Cargo.lock` package entry to the TOML updater in `release-please-config.json`. Release builds use `--locked`, so updating `Cargo.toml` without every corresponding lock entry breaks packaging.

## Updater Behavior

- `THREADLANE_UPDATER_PUBLIC_KEY` and `THREADLANE_UPDATER_ENDPOINT` are compile-time environment values through `option_env!`.
- Never hardcode private updater keys or passwords.
- Update checks and downloads may run from `cargo run`; installation must remain restricted to a packaged app bundle.
- Trigger an update check in the background on every application launch. Reveal notice UI only for an available or active update flow.
- Keep updater lifecycle states explicit: idle, checking, available, up to date, downloading, ready to install, installing, and error.
- Preserve target-version context during download progress.
- Download/install progress belongs in the dedicated notice UI, not as repeated system messages in the conversation.
- Keep status copy concise and truncate unbounded release notes or errors before placing them in compact UI.

## WASI Extensions

- Extension crates live under `extensions/` and target `wasm32-wasip1`.
- A loaded extension retains its compiled WASI module and creates only a fresh store/instance per invocation. Response lookup is registry-only; startup and explicit install/toggle reload signals own filesystem discovery, so a lookup miss must not recompile the inventory.
- Extension install, toggle, and removal must reject symlinked destination components and keep every mutation inside the selected global or project `.threadlane/extensions` root. Validate staged WASM and its embedded manifest before swapping it into place so installation cannot report failure after commit.
- Inventory and runtime loading share one scoped discovery path. Enabled project modules override enabled global modules with the same manifest name, while both rows remain visible in settings. Disabling a project override reveals an enabled global module.
- Use `./scripts/build_extensions.sh` to compile and deploy them.
- An extension that drives a long-lived subprocess uses the broker's named managed process (`process.spawn`/`send`/`recv`/`kill`), not `process.run`. The process outlives a single tool call, which is what lets `debug_ext` stop at a breakpoint in one call and resume in the next. `process.recv` supports `content-length` framing, so both LSP and DAP need no framing code of their own.
- Extension state is **one slot per extension**, persisted to disk and threaded into every invocation regardless of which tool ran. It is not per-tool-call scratch space. A tool that returns a terminal response without setting `state` leaves the previous phase persisted, and the next tool call then starts in a transient phase it cannot handle. Every terminal path must persist a stable state.
- Do not use the phase string to tell a new tool call from a continuation — a fresh call arrives carrying the previous call's phase. The reliable discriminator is whether the invocation carries `broker_response` events, as `debug_ext::is_continuation` does.
- Broker responses arrive on the *next* invocation as `broker_response` events, so any multi-step protocol exchange is a phase machine over `Invocation::state` with `continue_after_broker` set. Follow the `lsp_ext`/`debug_ext` shape: a `phase` string names what the extension is waiting for, and an unrecognized message re-issues the read without changing phase.
- A protocol that interleaves responses and events (DAP especially) needs a bounded pump. Count continuation steps in the extension's state and fail with a clear message at the cap; an adapter streaming `output` events would otherwise keep a tool call alive indefinitely.
- Declare the narrowest capability set that a tool actually needs. `debug_ext` requests only `process` even though it deals in file paths, because the adapter reads sources itself.
- Brokered network access requires approval for the exact lowercase host. GPUI sessions request approval through the generic session permission handle and can persist exact project-scoped hosts in `.threadlane/permissions.json`; unattended callers default to denial. `THREADLANE_NETWORK_ALLOW_HOSTS` remains a non-interactive preapproval path. HTTPS requests must keep response bodies bounded and redirects disabled; a redirect destination requires its own approval and an explicit follow-up fetch so redirects cannot bypass host policy.
- The script treats missing binaries and copy failures as fatal and must not clear user-installed modules or disabled markers from the extension root.
- Bundled agent definitions and prompts are part of a valid extension deployment; do not update only the `.wasm` artifact when associated metadata also changes.

## Security and Sensitive Files

- Never read, print, edit, or commit private keys, password files, access tokens, or local credentials unless the user explicitly requests a narrowly scoped security operation.
- The repository root may contain ignored local updater-key or password files. Treat them as secrets even when visible in directory listings.
- Public updater keys may be referenced by documented commands, but private signing material must remain outside source control.
- Do not log provider credentials or authentication responses containing secrets.

## Documentation

- Update `README.md` when changing build, updater, packaging, or local-testing workflows.
- Store README screenshots under `docs/images/` with descriptive filenames and alt text; use repository-relative links so they render on GitHub and in local Markdown previews.
- Keep command examples runnable from the repository root unless the text explicitly changes directories.
- Explain limitations that matter to users, especially compile-time updater configuration and packaged-app-only installation.

## Keep This Guide Current

- Treat `AGENTS.md` as living repository documentation.
- Whenever work reveals a new repository-specific convention, architectural constraint, recurring pitfall, required validation step, or non-obvious workflow, add it to the appropriate section of this file as part of the same change.
- Record durable lessons that will help future agents; do not add temporary task details, speculative guidance, or information already obvious from the code.
- Update existing guidance when behavior changes instead of leaving contradictory or obsolete instructions.

## Before Finishing

- Consider whether the task uncovered a durable lesson that belongs in `AGENTS.md`.
- Review the diff for accidental changes and generated files.
- Run the focused validation commands and report exactly what passed.
