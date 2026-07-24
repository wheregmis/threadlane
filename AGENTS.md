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

# Run the desktop app
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

### Registration and Namespaces

Reusable script components are registered through `crates/threadlane/src/components/mod.rs`.

- Initialize `mod.components` first via `components/init.rs`.
- Register dependencies before components that reference them.
- Add every new component module to both the Rust module list and the `script_mod(vm)` registration sequence.
- Use `mod.components.Name` for reusable templates.
- Use `:=` IDs for widgets that Rust code must retrieve with `ids!(...)`.

### Layout

- Explicitly set `width`, `height`, `flow`, `spacing`, `padding`, and alignment when they affect interaction geometry.
- Use one source of truth for visual and interactive bounds. Do not draw a hover rectangle from hard-coded coordinates while text/click handling lives in another widget.
- Fixed-height rows should vertically center their content with `align: Align{y: 0.5}`.
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
- The widget that owns text and click handling should also own its hover/pressed background.
- Use matching interaction and drawing bounds so the hover surface always contains the label.
- Include keyboard focus behavior when restyling interactive controls; do not optimize only for mouse hover.

### Overlays and Event Routing

Drawing in an overlay does not automatically stop widgets underneath from receiving pointer events.

- Context menus and popups must account for both visual stacking and event routing.
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

### Composer Drop-Ups

The pinned Makepad `PopupMenuPosition` currently supports only `OnSelected` and `BelowInput`; it has no native `AboveInput` variant.

Threadlane’s composer dropdown implementation relies on these invariants:

- The selected model or reasoning effort is reordered to the final label position.
- The final selected popup row is a transparent anchor.
- The visible popup surface ends above that anchor, leaving the closed picker visible.
- `EffortDropDown` and `ModelDropDown` use popup widths matching their trigger widths.

If changing ordering, row height, popup padding, or selected-item behavior, update the transparent-anchor geometry in `components/dropdown.rs` as part of the same change.

### Shaders and Colors

- Follow existing `#xRRGGBB` and `#xRRGGBBAA` color syntax.
- Keep custom SDF shaders simple and ensure dimensions cannot become negative; use `max(0.0, ...)` for computed sizes where appropriate.
- Shader uniforms and instance fields must be declared consistently with how they are animated or updated.
- `Sdf2d.fill_keep(...)` retains the current shape. Consume it with `stroke(...)` before constructing unrelated geometry, even when the stroke width is zero, or a later fill can repaint the retained shape.
- Compilation does not replace visual testing for shader geometry.
- Prefer subtle borders and state changes consistent with the existing dark interface. Avoid heavy glow, large shadows, or highly saturated surfaces unless requested.
- Do not rely on emoji or uncommon glyphs for critical status indicators; the current UI font may render them incorrectly. Prefer text, SVG resources, or simple drawn indicators.

## Session and Context-Menu Behavior

- The sidebar keeps secondary actions quiet: the project attach button appears while hovering the `PROJECTS` header, and per-project detach/new-session buttons appear while hovering that project row.
- Hover-revealed sidebar actions are synchronized from the current pointer position and actual widget bounds in `App::sync_sidebar_action_visibility`. Run this after `self.ui.handle_event(...)`, because Makepad updates hit-routing state while traversing the UI. Do not rely only on one-shot hover-enter/leave actions for recycled `PortalList` rows; redraws can otherwise leave child visibility stale.
- A hidden child has no drawable area to invalidate. When changing a hover-revealed child from hidden to visible, explicitly redraw its owning header or list row; otherwise it may not appear until an unrelated click triggers a broader redraw.
- Session rows are rendered by a `PortalList`; templates are selected from shared session state during draw.
- Session titles use the clipped `SessionTitle` marquee component. On title hover, only overflowing text scrolls to its measured end, pauses, resets to the beginning, and repeats; leaving the title immediately restores the starting position.
- The sidebar presents projects and sessions as a tree: project headers draw the parent stem, session rows draw child connectors, and the final session uses a terminating connector template. Session hover and active states use accent underlines across the bounded, ellipsized title column rather than filled pills so they never cover the tree; both states share identical geometry, with the active color taking precedence, and a filled row surface is reserved for the context target. Selecting a session highlights only that session; reserve the active project-header treatment for a project draft with no active session.
- The context-target state is distinct from the active-session state.
- Opening a session context menu sets the context target; closing it must always clear that target.
- Archive and delete actions should flow through `SessionContextMenuAction` and the app’s existing action handler.
- Keep popup row geometry, popup height constants, padding, and hit behavior synchronized.
- Do not allow a context-menu interaction to activate or hover an underlying session row.

## Updater Behavior

- `THREADLANE_UPDATER_PUBLIC_KEY` and `THREADLANE_UPDATER_ENDPOINT` are compile-time environment values through `option_env!`.
- Never hardcode private updater keys or passwords.
- Update checks and downloads may run from `cargo run`; installation must remain restricted to a packaged app bundle.
- Keep updater lifecycle states explicit: idle, checking, available, up to date, downloading, ready to install, installing, and error.
- Preserve target-version context during download progress.
- Updater status belongs in the dedicated header/notice UI, not as repeated system messages in the conversation.
- Keep status copy concise and truncate unbounded release notes or errors before placing them in compact UI.

## WASI Extensions

- Extension crates live under `extensions/` and target `wasm32-wasip1`.
- Use `./scripts/build_extensions.sh` to compile and deploy them.
- The script intentionally treats missing binaries and copy failures as fatal.
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
