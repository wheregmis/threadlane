# AGENTS.md

Guidance for coding agents working in the Threadlane repository.

## Scope

This file applies to the entire repository.

Threadlane is a Rust workspace centered on a native Makepad desktop application. Keep changes focused, preserve the existing visual language, and prefer established project patterns over introducing new frameworks or dependencies.

## Repository Map

- `crates/threadlane/` — native desktop application and primary binary.
  - `src/app/mod.rs` — application shell, top-level Makepad `script_mod!` UI, startup wiring, action handling, async event polling, workspace selection, and updater UI.
  - `src/components/` — reusable Makepad components and custom widgets.
  - `src/panels/chat/` — chat presentation, composer state, message rendering, and generation UI.
  - `src/panels/sessions/` — project/session list, persistence, registry, and sidebar behavior.
  - `src/state.rs` — shared application/session state and agent events.
  - `src/workspace.rs` — workspace-local state.
  - `src/updater.rs` — signed update checks, downloads, installation, and relaunch.
- `crates/threadlane-agent/` — agent runtime and event stream.
- `crates/threadlane-coding-agent/` — coding-agent orchestration, skills, subagents, and project context.
- `crates/threadlane-provider/` — model/provider and authentication integrations.
- `crates/threadlane-tools/` — tool implementations and capability support.
- `extensions/` — WASI extensions built for `wasm32-wasip1`.
- `scripts/build_extensions.sh` — builds and deploys bundled extensions, agents, and prompts into `.threadlane/`.
- `Makepad.md` — Makepad/Splash DSL notes and Liquid Glass reference. Use it as a syntax and design-pattern reference, but note that Threadlane is a native Rust/Makepad app with its own component system; do not blindly replace native widgets with Splash-only `glass.*` names.
- `packaging/`, `.github/workflows/`, and package metadata — release and platform packaging.

Do not edit generated content under `target/`, `crates/threadlane/dist/`, or deployed runtime content under `.threadlane/` unless the task explicitly concerns generated artifacts.

## Common Commands

Run commands from the repository root.

```bash
# Fast validation for desktop-app changes
cargo check -p threadlane

# Focused updater tests
cargo test -p threadlane updater::tests

# Full workspace tests
cargo test --workspace

# Build and deploy WASI extensions
./scripts/build_extensions.sh

# Run the desktop app directly only for non-visual debugging
cargo run -p threadlane

# Check patch whitespace

git diff --check
```

For a local updater UI check against the published manifest:

```bash
THREADLANE_UPDATER_PUBLIC_KEY="$(cat threadlane-updater.key.pub)" \
cargo run -p threadlane
```

A normal `cargo run` may be unsuitable for testing installation: update installation and relaunch are intentionally restricted to a packaged `.app`.

### Makepad Studio runtime debugging

Use Makepad Studio for Threadlane UI/runtime verification. The repository root
contains `makepad.splash`, which exposes the `threadlane` Cargo package as a
Studio runnable item.

Install and start Studio once on a machine with a working Metal device:

```bash
cargo install --git https://github.com/makepad/makepad makepad-studio --locked
makepad-studio --mounts=makepad:$PWD
```

In a second terminal, start the localhost bridge and keep it running for the
whole interaction:

```bash
cargo-makepad studio --studio=127.0.0.1:8001
```

Send newline-delimited JSON requests to the bridge. For a fresh visual run:

1. Send `{"ListBuilds":[]}` and clear any existing Threadlane build with
   `{"ClearBuild":{"build_id":[N]}}`.
2. Launch the current source with
   `{"RunItem":{"mount":"makepad","name":"threadlane"}}`.
3. Wait for `BuildStarted` and application startup before inspecting the app.
4. Use `{"WidgetTreeDump":{"build_id":[N]}}` for widget IDs and coordinates,
   `{"Screenshot":{"build_id":[N]}}` for visual evidence, and `Click`,
   `TypeText`, and `Return` for interaction checks.
5. After every UI/runtime edit, clear the old build and start a new Studio run;
   never validate a stale build. Inspect Studio build logs for script type-check
   errors because Makepad DSL failures can occur after Rust compilation.

Keep the bridge and Studio bound to localhost. Do not use `ObserveMount` or
bind Studio to `0.0.0.0` for ordinary debugging. Studio may create a local
`.makepad/` state directory; it is generated runtime state and must not be
edited or committed.

## Validation Expectations

1. Start with the narrowest relevant test or check.
2. For Rust or Makepad UI edits, run at least:
   - `cargo check -p threadlane`
   - `git diff --check`
3. Run focused tests for touched logic, then broader workspace tests when warranted.
4. Makepad script and shader behavior can have runtime-only visual issues even when Rust compilation succeeds. For layout, hover, popup, or shader changes, state when visual runtime verification is still needed.
5. Do not claim a UI behavior was visually verified unless the application was actually run and observed.
6. Existing unused-code warnings are not part of unrelated tasks; do not remove meaningful code merely to silence them.

## Rust and Architecture Conventions
- **Strict reuse gate:** Before implementing anything, search the repository for an existing component, helper, state type, command path, or dependency that already provides the needed behavior. Reuse or extend the existing implementation whenever possible. Do not create a duplicate component, abstraction, utility, or parallel state path unless you document why the existing one cannot satisfy the requirement.
- Prefer `edit_file_hashline` for high-precision edits using line:hash anchors (e.g. '12:a3f') returned from `read_file` to ensure edit safety and prevent line drift. Use range edits (`start_anchor` to `end_anchor`) for multi-line replacements/deletions, batch multiple edits into a single tool call, and re-read the target range with `read_file` if a hash mismatch occurs.

- Keep edits surgical. Do not move unrelated symbols or reformat large files without need.
- Reuse existing dependencies and runtime infrastructure.
- Preserve separation between reusable components, panel-specific behavior, shared state, and the top-level app shell.
- Put reusable visual primitives in `crates/threadlane/src/components/` rather than growing `app/mod.rs` further when a component has independent behavior.
- Keep chat behavior in `panels/chat/` and session/sidebar behavior in `panels/sessions/` when practical.
- Prefer root-cause fixes over state-specific offsets or visual patches.
- Avoid holding locks across expensive work, UI callbacks, or async boundaries.
- Background tasks communicate with the UI through channels and call `SignalToUI::set_ui_signal()` after sending state. Follow that pattern rather than updating widgets from worker threads.
- UI updates belong on the Makepad event thread. Update state first, synchronize widget refs, and request redraws only where needed.
- Preserve user work and persisted session data. Never casually delete `.threadlane` state or session files.

## Makepad Component Conventions

### Theme Colors

- All UI colors in crates/threadlane/src must reference role-based `theme.*` tokens (`background`, `foreground`, `card`, `primary`, `secondary`, `muted`, `accent`, `destructive`, `success`, `warning`, `border`, `input`, `ring`); new color literals belong only in crates/threadlane/src/theme/mod.rs.
- Do not add one-off hue or component tokens when an existing semantic role fits; add a role only when it represents a reusable UI meaning.
- Preserve explicit hover, focus, pressed, border, transparency, and alpha variants when migrating or adding theme tokens.

### Registration and Namespaces

Reusable script components are registered through `crates/threadlane/src/components/mod.rs`.

- Initialize `mod.components` first via `components/init.rs`.
- Register dependencies before components that reference them.
- Add every new component module to both the Rust module list and the `script_mod(vm)` registration sequence.
- Use `mod.components.Name` for reusable templates.
- Use `:=` IDs for widgets that Rust code must retrieve with `ids!(...)`.
- Custom widgets should handle actions from their deeply nested child controls locally and emit a typed `cx.widget_action(...)` for the app shell. Do not rely on root-level widget lookup to resolve controls inside a custom widget's dereferenced view. Check for child button actions inside `if let Event::Actions(actions) = event` using `self.view.button(cx, ids!(btn_id)).clicked(actions)` rather than listening for raw pointer events.
- A custom widget that dereferences a `View` must delegate both `handle_event` and `draw_walk`; delegating events without drawing leaves the entire wrapper invisible.
- **DSL inheritance**: `:= SomeName { ... }` creates an ID-bound widget instance, **not** a named prototype. An ID-bound instance cannot be used as a parent in another `:= SomeName { ... }` definition. Only `mod.components.Name` template names (defined with `=`, not `:=`) are valid prototype parents. Attempting to write `Child := ParentId { ... }` where `ParentId` was defined with `:=` will fail at runtime with "variable ParentId not found in scope".

Navigation destinations that are siblings in the same menu should use
`mod.components.NavButton`. Keep ordinary actions, icon buttons, dropdowns,
and destructive controls as their own variants; do not recreate navigation
hover, focus, pressed, border, or selected styling at each call site.

### Layout

- Explicitly set `width`, `height`, `flow`, `spacing`, `padding`, and alignment when they affect interaction geometry.
- Use one source of truth for visual and interactive bounds. Do not draw a hover rectangle from hard-coded coordinates while text/click handling lives in another widget.
- Fixed-height rows should vertically center their content with `align: Align{y: 0.5}`.
- A fixed-height Makepad `Label` needs its own vertical `align`; parent alignment positions the label widget but does not center glyphs inside the label's draw walk. Start with `Align{y: 0.5}`, clear inherited padding with `padding: 0`, and adjust the label's `align.y` only when the font's ascender/baseline metrics remain optically off-center.
- Makepad `DropDownFlat` defaults to top-left alignment. Closed composer dropdowns must explicitly preserve left alignment and set vertical centering:

```text
align: Align{x: 0.0 y: 0.5}
```

- Avoid oversized corner radii at small control heights. A radius equal to half the height can produce distorted or pointed shapes in some Makepad button shaders. Existing compact controls generally use radii around `5–8`.
- Prefer concise text that does not force a compact header control to grow unexpectedly.

### Icon-Only Buttons

Makepad `Button` reserves default spacing between its icon and text, even when `text: ""`. This makes a mathematically centered button render its SVG left of center.

Every icon-only `Button`, `ComposerChip`, or `ComposerAction` should normally include:

```text
padding: 0
spacing: 0
text: ""
align: Align{x: 0.5 y: 0.5}
```

Keep the SVG view box itself centered. Do not compensate for inherited empty-label spacing by editing a correctly centered SVG or adding arbitrary margins.

### Hover, Focus, and Pressed States

- Define all relevant button states together: `color`, `color_hover`, `color_focus`, and `color_down`.
- If borders should remain invisible, also set all border-state colors explicitly. Inherited focus colors can otherwise appear unexpectedly.
- Standard Makepad `Button` does not expose a top-level `cursor` script property; it already supplies pointer cursor behavior through its interaction state.
- Toggle a button's runtime visibility through a typed `.button(...)` reference. Generic `.widget(...).set_visible(...)` dispatches the default no-op because `Button` does not override `Widget::set_visible`.
- The widget that owns text and click handling should also own its hover/pressed background.
- Use matching interaction and drawing bounds so the hover surface always contains the label.
- Include keyboard focus behavior when restyling interactive controls; do not optimize only for mouse hover.
- When a pointer action should move focus to another control, assign that focus after `self.ui.handle_event(...)`; assigning it before UI event dispatch allows the clicked surface to take focus back during the same mouse event. For press-driven controls, defer the request until primary mouse-up and use the target component's pending-focus API when available so the release phase cannot immediately clear it.
- Makepad `TextInput` cursor indices are UTF-8 byte offsets. To place the caret at the end of inserted text, use `text.len()`, not `text.chars().count()`; multibyte characters otherwise leave the caret before the final characters.

### Overlays and Event Routing

Drawing in an overlay does not automatically stop widgets underneath from receiving pointer events.

- Context menus and popups must account for both visual stacking and event routing.
- `DockFlat` draws its configured dock tree, not arbitrary extra children declared inside it. Instantiate pass-wide overlay widgets inside a branch the dock actually draws (for example, a dock-tab template) and give the overlay a dedicated `DrawList2d`; a later window-body sibling after `DockFlat` can remain uninstantiated and unresolved by ID. A raw full-window overlay `View` can also obscure the dock even when intended to start hidden.
- In an overlay, a later full-size sibling such as an empty `PortalList` can intercept pointer events intended for visible content beneath it. Hide inactive full-size siblings or place the interactive surface above them.
- The session context menu uses real child buttons for row hover/click states; do not reintroduce a parent shader with hard-coded row coordinates.
- While a session context target is active, the session list intentionally suspends its own event handling so rows under the popup cannot also hover or press.
- Be careful with `sweep_lock`: standard `Button` uses `event.hits(...)` with the default sweep area. Locking a different area can prevent popup buttons from receiving events unless those controls explicitly use `hits_with_sweep_area`.
- Outside-click dismissal, Escape, and back navigation should all close overlays and clear associated state.
- Clamp popup coordinates to the pass bounds and keep a small edge gap.

### Command Completion Popup

- Command rows use a fixed height and the `PortalList` viewport height is derived from the number of visible results, capped at a small maximum. Do not restore a large fixed viewport that leaves empty popup space.
- Keep the active marker, command name, and description in one fixed-height row. The marker and command name must share matching font metrics and vertical alignment; use a bounded, ellipsized command-name column and let the description consume the remaining width.
- Keyboard Up/Down navigation wraps at both ends.
- When rebuilding or clearing filtered results, reset both the first item and its pixel offset with `set_first_id_and_scroll(0, 0.0)`. `set_first_id(0)` alone preserves stale `first_scroll` and can vertically offset a short result list.
- Makepad `PortalList::smooth_scroll_to` stops when a target row’s top edge is visible, even if the row is not fully revealed. When wrapping from the first command to the final command, use `scroll_to_end` so the selection and viewport reach the actual bottom.
- Keep keyboard focus and pointer hover as separate states; keyboard movement should clear pointer hover before redrawing.

### Chat Transcript

- User submissions remain right-aligned chat bubbles. Assistant responses render as flat markdown aligned with the transcript rather than enclosed cards.
- Bounded `Fit` markdown computes its intrinsic line width before applying the cap, so long pasted lines can clip. Keep short user messages on the compact `UserMsg` template and route long lines through `UserMsgWrapped`, whose bounded `Fill` bubble and `Fill` markdown wrap within the viewport.
- Makepad Markdown tables divide the current concrete inner width among their columns. Assistant markdown must use bounded `Fill`, not bounded `Fit`; an unresolved `Fit` width collapses table cells into tall invisible strips.

### Chat Activity Groups

- Consecutive thinking and tool messages are grouped into one collapsible `Working`/`Worked` display row by `panels/chat/view.rs`.
- Grouping is presentation-only: preserve the underlying `ChatMessage` entries so tool output and persisted session history remain intact.
- Streaming thinking merges into the trailing activity group; streaming assistant text remains a normal assistant message.
- Treat `AgentEvent::MessageUpdate.tool_call_name` as the semantic boundary for an assistant tool-call preamble. Flush buffered assistant text into the activity group at that event rather than waiting to infer it from `MessageEnd`.
- Aborting a generation suppresses the normal stream and tool-end events. Commit partial streaming content, mark running tools cancelled, clear the session working state, and redraw the session list so no activity loader remains visible.
- Keep summary categories concise and action-oriented (`Explored`, `Edited`, `Ran`, `Loaded`, `Delegated`) and bound expanded tool output rather than restoring every raw tool payload to the top-level transcript.
- Expanded activity groups must render complete persisted thinking segments and current streaming reasoning in event order alongside concise tool summaries; never replace finalized reasoning with only a completion/status placeholder.
- Opening or closing `ToolFoldHeader` changes its `PortalList` item height. Preserve its `LayoutChanged` action and redraw the parent chat list so following messages are reflowed instead of overlapping the expanded body.
- `SubagentRail` manually draws dynamic `ToolFoldHeader` rows but does not keep per-row draw state. Consume each row with `draw_all_unscoped`; propagating a row's intermediate `DrawStep` restarts the rail loop at the first header and prevents expanded bodies from drawing.
- Custom containers that manually draw dynamic children need their own valid hit area. Prefer an instance-backed transparent draw primitive for that area; retaining a raw turtle `Area::Rect` across recycled `PortalList` redraws can leave a stale `rect_id` and panic during pointer movement.
- The chat `PortalList` range is based on display rows, not raw message count. If changing grouping, preserve stable ordering, auto-tail behavior, and non-reused fold widget state.
- A `PortalList` can yield a stale visible item ID for one draw after its range shrinks. Resolve dynamic backing rows with `.get(item_id)` and skip missing entries instead of indexing directly.
- Avoid calling `markdown.set_text(cx, text)` unconditionally inside `draw_walk` when text is unchanged; `set_text` forces Makepad to discard its AST and re-parse Markdown/syntax highlighting on every frame draw. Check `widget.text() != text` before updating.
- Precalculate tool details, activity summaries, and JSON presentations in `DisplayRow` state when building display items rather than invoking `serde_json::from_str` or string line-splitting during `draw_walk`.
- Cross-session task UI reads the canonical `HarnessSupervisor` registry. Keep detached `/task` work and foreground model-subagent lifecycle there rather than adding another GUI-only counter; model subagents remain synchronous and emit lifecycle events only for observation.

### Composer Drop-Ups

The pinned Makepad `PopupMenuPosition` currently supports only `OnSelected` and `BelowInput`; it has no native `AboveInput` variant. `OnSelected` is the Rust enum's `#[pick]` default, but `PopupMenuPosition.OnSelected` is not available in component script scope, so omit the DSL property when selecting that default.

Threadlane’s composer dropdown implementation relies on these invariants:

- The selected model or reasoning effort is reordered to the final label position.
- Keep the canonical model list separate from that temporary display order. Repeated selections must not write the selected-last anchor order back into provider ordering; OpenAI and Antigravity entries remain grouped.
- The final selected popup row is a transparent anchor.
- The visible popup surface ends above that anchor, leaving the closed picker visible.
- `EffortDropDown` and `ModelDropDown` use popup widths matching their trigger widths.
- The stock Makepad `PopupMenuItem` is text-only. `components/model_dropdown.rs` owns the shared icon-aware trigger and popup-row implementation used by both `ModelDropDown` and `EffortDropDown`.
- Model rows select OpenAI or Google SVGs from the persisted model prefix; effort rows always use the reasoning SVG. Keep effort labels as raw `ReasoningEffort` values so parsing remains unchanged.
- The model list is the union of dynamically fetched OpenAI models and models for other authenticated providers. OpenAI refresh events must preserve connected-provider entries, and successful provider login should update the picker immediately.
- Model-picker changes run as internal `/model` commands. They must not consume, clear, or restore over the current composer draft and image attachments; only actual composer submissions own the submitted-draft and submitted-attachment lifecycle.

If changing ordering, row height, popup padding, or selected-item behavior, update the transparent-anchor geometry in `components/model_dropdown.rs`.

### Shaders and Colors

- Follow existing `#xRRGGBB` and `#xRRGGBBAA` color syntax.
- A custom `View` widget does not inherit `RoundedView` shader fields. If its shader references `border_size` or `border_color`, declare them explicitly with `uniform(...)`; Rust compilation does not catch missing script shader fields.
- Keep custom SDF shaders simple and ensure dimensions cannot become negative; use `max(0.0, ...)` for computed sizes where appropriate.
- Shader uniforms and instance fields must be declared consistently with how they are animated or updated.
- `Sdf2d.fill_keep(...)` retains the current shape. Consume it with `stroke(...)` before constructing unrelated geometry, even when the stroke width is zero, or a later fill can repaint the retained shape.
- Compilation does not replace visual testing for shader geometry.
- Prefer subtle borders and state changes consistent with the existing dark interface. Avoid heavy glow, large shadows, or highly saturated surfaces unless requested.
- Do not rely on emoji or uncommon glyphs for critical status indicators; the current UI font may render them incorrectly. Prefer text, SVG resources, or simple drawn indicators.

## Session and Context-Menu Behavior

- Project terminal groups are keyed by canonical project work directory, not by session ID. Each project can own multiple independent shell tabs; switching sessions in one project must retain its shells, active tab, and output, while switching projects selects that project's terminal group.
- Model-managed todo plans are session-scoped and persisted as complete `session_plan` records in the existing session JSONL. Show only the active session's plan above the project-wide task groups; do not derive plan state from compactable tool-call history or merge it into supervisor task state.

- The project attach button appears while hovering the `PROJECTS` header. It is the only sidebar action synchronized from `App::sync_sidebar_action_visibility` and the retained app-level pointer.
- `ProjectHeader` locally owns project-row hover hit-testing, action-button paint/redraw, and typed select/new/detach actions for both fixed and portal-list headers. Keep its controls laid out inside the fixed-width action slot; do not restore app-level project-row geometry synchronization.
- Compute `ProjectHeader` hover from `MouseMove` against the header's clipped rectangle. Parent `FingerHoverOut` can fire when the pointer enters a nested button, which must not hide the action group.
- Keep project collapse clicks on the bounded `project_toggle_surface` child and consume its `finger_up` action after child event dispatch. Post-dispatch hit-testing on the parent header can miss pointers already handled by its inner view.
- A hidden child has no drawable area to invalidate. When changing a hover-revealed child from hidden to visible, explicitly redraw its owning header or list row; otherwise it may not appear until an unrelated click triggers a broader redraw.
- Session rows are rendered by a `PortalList`; templates are selected from shared session state during draw.
- The session sidebar feeds `PortalList` one flattened visible-row stream derived from runtime project tree state. Expanded projects show four preview sessions, substituting an older active session for the fourth item; overflow and collapsed rows must be represented in that stream rather than hidden only at draw time.
- Clicking a project header toggles its children. Collapsing clears that project's show-all state, so reopening returns to the four-session preview; detach and new-session child buttons remain independent of the toggle hit area.
- Session titles use the clipped `SessionTitle` marquee component. On title hover, only overflowing text scrolls to its measured end, pauses, resets to the beginning, and repeats; leaving the title immediately restores the starting position.
- The sidebar presents projects and sessions as a tree: project headers draw the parent stem, session rows draw child connectors, and the final session uses a terminating connector template. Session hover and active states use subtle filled surfaces without accent underlines; keep those surfaces to the right of the tree so connectors remain visible. Selecting a session highlights only that session; reserve the active project-header treatment for a project draft with no active session.
- The context-target state is distinct from the active-session state.
- Opening a session context menu sets the context target; closing it must always clear that target.
- Archive and delete actions should flow through `SessionContextMenuAction` and the app’s existing action handler.
- Keep popup row geometry, popup height constants, padding, and hit behavior synchronized.
- Do not allow a context-menu interaction to activate or hover an underlying session row.

## Model Provider Routing

- Provider selection is encoded in the persisted model ID. Models prefixed with `antigravity/` or `opencode-go/` route through `threadlane-provider::router::ProviderClient`; unprefixed models retain the OpenAI path. Preserve the prefix across model switching, sessions, subagents, and payload construction.
- Persist each session's selected model in `SessionTree` metadata. Restore it before constructing the agent runtime and synchronize the model picker from that restored value; legacy metadata without a model continues to use the caller-provided default.
- A restored session has two synchronized representations: the persisted `SessionTree` active branch and `AgentState.messages`, which supplies provider context. Every constructor or session-switch path must load the active branch into `AgentState.messages` after the current system prompt; populating only the chat UI makes old messages visible without sending them to the model and also breaks subsequent prefix-based persistence.
- Keep the central agent loop provider-neutral. Provider clients must translate requests and stream results into the shared `StreamEvent`, `ToolCall`, and `ProviderUsage` contract so tool execution, hooks, compaction, persistence, and chat rendering are not duplicated.
- OpenAI Responses events distinguish streaming `*.delta` events from final `*.done` snapshots. Emit only explicit text/reasoning deltas; never pass `response.*.done` fields through generic text fallbacks, or final output is duplicated and reasoning snapshots can leak into assistant content.
- Antigravity uses Google Cloud Code Assist's `v1internal` endpoints and outer request envelope, not the public Gemini `streamGenerateContent` endpoint. Preserve project discovery, production/daily endpoint fallback, runtime-model mapping, wrapped SSE parsing, and provider-specific tool schemas when changing that client.
- Gemini tool calls can include a required `thoughtSignature`. Preserve it on the shared persisted `ToolCall` and replay it on the assistant `functionCall` part; dropping it causes the next tool-result request to fail with HTTP 400.
- Credential checks follow the selected model. Antigravity models require stored Antigravity OAuth credentials but must not require an OpenAI key; OpenAI models retain the existing OpenAI credential requirement.
- Automatic session titles must route through `ProviderClient` so provider-prefixed models use their own credentials and request format. OpenCode titles use streamed Chat Completions even when an OpenAI ChatGPT/Codex account is also configured; skip the title side path for Antigravity sessions rather than consuming an OpenAI credential or permanently marking a failed Antigravity title attempt.

## Background Tasks and Capabilities

- `HarnessSupervisor` owns only explicit background tasks (currently `/task <prompt>`). Ordinary chat sessions continue to use the existing `SessionRuntime` path and must not be mirrored into supervisor tasks.
- Harness side effects are intent-first: persist `OperationStarted`, `TaskAttempt`, `ToolStarted`, and `QueueEnqueued` under the lane lock before starting the corresponding model/tool work or mutating the in-memory queue. `ToolExecutionStart` is observational only.
- Child intent is durable before model/tool work; checkpoints use `WriteDeferred`; safe replay is automatic and unsafe interruption aborts.
- Model subagents execute with short-lived child `Agent`s but persist as passive sibling branches on the parent `SessionTree`; only the formatted final tool result enters the parent active branch.
- Forward supervisor events through `GuiAgentEvent`; update `BackgroundTaskState` and widgets only on the Makepad event thread.
- Threadlane extensions are compiled WASI modules with an exported
  `extension_info` manifest. The settings picker installs a `.wasm` into either
  `~/.threadlane/extensions/` or `<project>/.threadlane/extensions/`; it never
  runs Cargo or extension build scripts. Native extension executables and trust
  approvals are unsupported. LSP remains a WASI extension and launches language
  servers through brokered process capability.

### Project-Scoped Skill Enable/Disable

- Skills are toggled per project, not globally. `SkillSettings` persists disabled skill IDs in `<project>/.threadlane/skills.json`; skill discovery (`Discovery::finish`) applies those overrides so a disabled skill stays visible in the settings list with `enabled: false` but is excluded from the model catalog and rejected by `load_skill`.
- The settings modal has a dedicated `skills_page` (separate `PortalList` and row template from `capability_list`). In `ProviderSettingsModal::draw_walk`, the two `PortalList`s share one `as_portal_list()` loop, so each branch must be matched by comparing `list.widget_uid()` against the resolved list widget before drawing rows.
- A toggle must clear `capability_cache`, refresh the capabilities chip / slash commands via `refresh_project_capabilities`, and call `refresh_live_session_skills` so running sessions re-discover skills. `CodingAgent::refresh_skills` swaps the shared `SkillRegistry` `Arc`; note the already-registered `LoadSkillToolExecutor` holds the previous `Arc`, so an in-flight session keeps the catalog from its creation and a fresh session fully reflects the toggle.

## External ACP Agents

- Threadlane is an Agent Client Protocol *client*: it launches a third-party agent as a subprocess and speaks newline-delimited JSON-RPC 2.0 over its stdio pipes. It is not an ACP agent server, and ACP has no non-stdio transport, so an `AcpAgentConfig` is always a spawnable command.
- `crates/threadlane-coding-agent/src/acp.rs` owns the protocol. Follow the `mcp.rs` precedent rather than adding a protocol SDK dependency: the wire format is hand-rolled with `serde_json` over `tokio::process`, which keeps the runtime model consistent with the rest of the workspace.
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
- An ACP agent is selected as a model id of the form `acp/<agent_id>`, reusing the `antigravity/` prefix convention so it flows through the existing picker, `/model`, and per-session model persistence. `append_acp_models` injects them inside `set_model_dropup_options` so every path that repopulates the picker includes them.
- ACP session updates are mapped onto `AgentEvent` in `acp_bridge`, not rendered through a parallel path. Keep that mapping pure and in the coding-agent crate so it stays testable without a `Cx`; the UI only forwards what it produces.
- Stopping an ACP turn must send `session/cancel` as well as aborting the task. Aborting only stops Threadlane listening; the external agent keeps working.
- The settings modal has a dedicated `acp_page` whose scope buttons share the modal's single `install_scope_global` flag with the WASI and MCP pages; register new scope buttons in both `sync_install_scope` and the scope-click handlers rather than adding a second scope flag.
- `ProviderSettingsModal::draw_walk` selects a `PortalList` branch by `self.page`, which is only correct because non-selected pages are invisible and therefore never draw. Any new capability page must add its branch there and its widget to `sync_page_visibility`, or its rows silently render into another page's list.
- `refresh_acp_state` renders configured agents from disk immediately with `Connecting` status, then replaces them when the background probe reports through `AcpRefreshCompleted`. Probing spawns processes and each handshake can take seconds, so never probe on the UI thread and never leave the list blank while it runs.
- Opening the settings modal must not probe ACP agents. Probing starts only when the ACP page is selected or refreshed, so merely opening settings never launches third-party binaries.

## Code Editor

- The embedded editor uses `makepad-code-editor` from the same pinned Makepad git revision as `makepad-widgets`. Do not vendor a copy of it: its only dependency is the sibling `makepad-widgets` crate, so a vendored copy would drift from the pinned revision on the `dev` branch and break at a Makepad bump.
- `CodeEditorView` follows Makepad Studio's `DesktopCodeEditor`: the upstream `CodeEditor` is not an ordinary auto-drawn child, because both drawing and event handling need a `CodeSession` threaded through. Studio keeps sessions in shared app data keyed by dock tab; Threadlane shows one file at a time, so the widget owns its session and needs no scope plumbing.
- That wrapper has no `#[walk]` of its own and delegates `walk()` to the inner editor, so setting `width`/`height` on `mod.components.CodeEditorView` fails at runtime with "property width not defined on type". Size the inner `editor` instead; its DSL default is already Fill/Fill.
- The wrapper's `script_mod!` must `use mod.widgets.*`, because `CodeEditor` is registered into `mod.widgets` by `makepad_code_editor::script_mod`, which has to run before the component that inherits from it. Both of these are runtime-only failures that `cargo check` cannot catch — run the app after touching editor DSL.
- Dirty state comes from `CodeEditorAction::TextDidChange`, not from "the editor returned some action". Cursor movement, selection, scrolling, and focus changes all return actions, so a looser check marks a byte-identical file as unsaved.
- A widget that emits a `cx.widget_action` needs a matching handler in the app shell or the signal goes nowhere; `CodeEditorViewAction::Modified` refreshes the editor header so the unsaved marker appears as the user types.
- Editor file loading refuses directories, non-UTF-8 content, and files over `MAX_EDITABLE_BYTES`. Keep that policy in `load_editable_text` rather than at the call site so it stays testable without a `Cx`.
- Saving writes `Text`'s `Display` form. It round-trips byte-for-byte, so do not "normalize" line endings on save; a covering test guards this.

## Performance

- Measure before changing. `crates/threadlane-mcp/tests/perf_baseline.rs` and `crates/threadlane-agent/tests/perf_baseline.rs` are `#[ignore]`d measurement harnesses, not assertions; run them with `-- --ignored --nocapture` to get a baseline and again to prove a change helped. Do not optimize a path whose cost has not been measured.
- Beware first-exec cost when benchmarking spawned processes. The first execution of a freshly written script costs ~200ms on macOS (a one-time system check), which lands inside whatever you are timing and reads as a product problem. `perf_baseline.rs` warms the stub up first; MCP discovery is ~5.5ms, not the ~200ms an unwarmed harness reports.
- UI latency is measurable with `THREADLANE_PERF=1`, which turns on `crate::perf` frame timing and prints a p50/p95/p99/jank summary every five seconds. Use it before claiming a UI path is slow or fixed, and measure a release build — debug figures are not representative.
- Pin a performance fix with a *behavioral* test, not a timing one. `tests/session_reuse.rs` counts how many server processes actually start, so it fails for the right reason on a loaded CI machine.
- MCP servers are long-lived: `McpManager` keeps one `McpSession` per server id and reuses it across tool calls. Do not reintroduce spawn-per-call — it cost ~5 ms per call against a trivial shell stub and far more against a real `npx`-based server. A failed exchange retires the session so the next call reconnects, and `Command::kill_on_drop` cleans up when the manager drops.
- Each MCP session carries its own lock. Hold the session map only long enough to look up or install a handle, never across the request round trip, or tool calls to unrelated servers serialize behind each other.
- Session files are parsed once through the untagged `SessionLine` enum. Do not go back to trying `SessionRecord` and then `SessionNode`, which parsed the JSON text of every node line twice.

## Updater Behavior

- `THREADLANE_UPDATER_PUBLIC_KEY` and `THREADLANE_UPDATER_ENDPOINT` are compile-time environment values through `option_env!`.
- Never hardcode private updater keys or passwords.
- Update checks and downloads may run from `cargo run`; installation must remain restricted to a packaged app bundle.
- Trigger an update check in the background on every application launch. Keep the sidebar update action hidden while idle, checking, up to date, or after a silent automatic-check error; reveal it only for an available or active update flow.
- Keep updater lifecycle states explicit: idle, checking, available, up to date, downloading, ready to install, installing, and error.
- Preserve target-version context during download progress.
- The update action belongs in the Projects sidebar. Download/install progress belongs in the dedicated notice UI, not as repeated system messages in the conversation.
- Keep status copy concise and truncate unbounded release notes or errors before placing them in compact UI.

## WASI Extensions

- Extension crates live under `extensions/` and target `wasm32-wasip1`.
- Extension install, toggle, and removal must reject symlinked destination
  components and keep every mutation inside the selected global or project
  `.threadlane/extensions` root. Validate staged WASM and its embedded manifest
  before swapping it into place so installation cannot report failure after
  commit.
- Inventory and runtime loading share one scoped discovery path. Enabled project
  modules override enabled global modules with the same manifest name, while
  both rows remain visible in settings. Disabling a project override reveals an
  enabled global module.
- Use `./scripts/build_extensions.sh` to compile and deploy them.
- An extension that drives a long-lived subprocess uses the broker's named managed process (`process.spawn`/`send`/`recv`/`kill`), not `process.run`. The process outlives a single tool call, which is what lets `debug_ext` stop at a breakpoint in one call and resume in the next. `process.recv` supports `content-length` framing, so both LSP and DAP need no framing code of their own.
- Extension state is **one slot per extension**, persisted to disk and threaded into every invocation regardless of which tool ran. It is not per-tool-call scratch space. A tool that returns a terminal response without setting `state` leaves the previous phase persisted, and the next tool call then starts in a transient phase it cannot handle. Every terminal path must persist a stable state.
- Do not use the phase string to tell a new tool call from a continuation — a fresh call arrives carrying the previous call's phase. The reliable discriminator is whether the invocation carries `broker_response` events, as `debug_ext::is_continuation` does.
- Broker responses arrive on the *next* invocation as `broker_response` events, so any multi-step protocol exchange is a phase machine over `Invocation::state` with `continue_after_broker` set. Follow the `lsp_ext`/`debug_ext` shape: a `phase` string names what the extension is waiting for, and an unrecognized message re-issues the read without changing phase.
- A protocol that interleaves responses and events (DAP especially) needs a bounded pump. Count continuation steps in the extension's state and fail with a clear message at the cap; an adapter streaming `output` events would otherwise keep a tool call alive indefinitely.
- Declare the narrowest capability set that a tool actually needs. `debug_ext` requests only `process` even though it deals in file paths, because the adapter reads sources itself.
- The script treats missing binaries and copy failures as fatal and must not
  clear user-installed modules or disabled markers from the extension root.
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
- Whenever work reveals a new repository-specific convention, architectural constraint, Makepad behavior, recurring pitfall, required validation step, or non-obvious workflow, add it to the appropriate section of this file as part of the same change.
- Record durable lessons that will help future agents; do not add temporary task details, speculative guidance, or information already obvious from the code.
- Update existing guidance when behavior changes instead of leaving contradictory or obsolete instructions.

## Before Finishing

- Consider whether the task uncovered a durable lesson that belongs in `AGENTS.md`.
- Review the diff for accidental changes and generated files.
- Confirm new component modules are registered.
- Confirm widget IDs used from Rust exist and remain uniquely addressable.
- Check icon-only buttons for `spacing: 0`.
- Check popup and overlay changes for underlying event leakage.
- Run the focused validation commands and report exactly what passed.
