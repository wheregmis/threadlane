# Adaptive Warm-Studio Prompt Composer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the existing mypi prompt composer a warm-studio visual system and adaptive idle, focused, command, working, and error presentations without changing agent behavior.

**Architecture:** Keep `MypiCommandTextInput` as the owner of editing and slash-command completion. Add a small pure presentation-state model under the chat panel for testable state-to-visibility decisions, and have `App` apply those decisions to the existing Makepad widgets from its current status/event path. Centralize warm colors in reusable primitives and apply the same surface/radius/selection treatment to the command popup.

**Tech Stack:** Rust 2021, Makepad Widgets DSL, existing `MypiCommandTextInput`, built-in Rust unit tests, Cargo.

## Global Constraints

- This phase is limited to the prompt composer and its slash-command popup.
- Existing text input, Enter/Shift+Enter behavior, command completion, model selection, plan controls, and stop behavior must remain functional.
- Do not add agent capabilities, context attachments, permissions management, token counters, or a new composer widget architecture.
- The current `MypiCommandTextInput` remains responsible for text editing and command completion.
- Use deep warm charcoal, terracotta, amber, warm off-white, tan-gray, and muted coral-red as the composer visual language.
- Failed requests retain user input and show an error presentation; a new request clears the prior error presentation.
- Changes must not overwrite or revert unrelated pre-existing worktree modifications.

---

## File Map

- Modify: `crates/mypi-gui/src/panels/chat/mod.rs` — expose the new pure composer presentation module.
- Create: `crates/mypi-gui/src/panels/chat/composer.rs` — state model and pure transition/visibility helpers with unit tests.
- Modify: `crates/mypi-gui/src/app/mod.rs` — apply composer layout, colors, labels, visibility, and status transitions to existing widgets.
- Modify: `crates/mypi-gui/src/components/primitives.rs` — define reusable warm-studio surface and accent values/patterns used by the composer.
- Modify: `crates/mypi-gui/src/panels/command_palette/view.rs` — restyle popup, command rows, and active selection using the warm-studio treatment.
- Modify: `crates/mypi-gui/src/panels/command_palette/state.rs` only if the existing completion tests need a pure selection-state assertion; do not change completion semantics.

No new dependencies or persistence files are required.

---

### Task 1: Add a testable composer presentation state

**Files:**
- Create: `crates/mypi-gui/src/panels/chat/composer.rs`
- Modify: `crates/mypi-gui/src/panels/chat/mod.rs`

**Interfaces:**
- Produces `ComposerStatus`, `ComposerPresentation`, `ComposerState`, and pure methods used by `App`.
- `ComposerState::new() -> ComposerState` starts in ready/idle presentation.
- `ComposerState::set_status(&mut self, status: ComposerStatus, message: impl Into<String>)` updates working/error/ready state and preserves the error message until a ready or working transition.
- `ComposerState::set_focused(&mut self, focused: bool)` updates focus presentation.
- `ComposerState::set_has_text(&mut self, has_text: bool)` updates typing presentation.
- `ComposerState::presentation(&self) -> ComposerPresentation` returns booleans and copyable presentation values for widget updates.

- [ ] **Step 1: Write the failing unit tests**

Add a `#[cfg(test)] mod tests` in `composer.rs` covering the state contract:

```rust
#[test]
fn idle_is_compact_and_hides_adaptive_controls() {
    let state = ComposerState::new();
    assert_eq!(state.presentation(), ComposerPresentation {
        expanded: false,
        show_model: false,
        show_plan: false,
        working: false,
        show_error: false,
        status_text: String::new(),
    });
}

#[test]
fn focus_expands_and_reveals_model_without_forcing_plan() {
    let mut state = ComposerState::new();
    state.set_focused(true);
    let presentation = state.presentation();
    assert!(presentation.expanded);
    assert!(presentation.show_model);
    assert!(!presentation.show_plan);
}

#[test]
fn typing_expands_composer() {
    let mut state = ComposerState::new();
    state.set_has_text(true);
    assert!(state.presentation().expanded);
    assert!(state.presentation().show_model);
}

#[test]
fn working_replaces_send_state_and_clears_old_error() {
    let mut state = ComposerState::new();
    state.set_status(ComposerStatus::Error, "Provider unavailable");
    state.set_status(ComposerStatus::Working, "Working...");
    let presentation = state.presentation();
    assert!(presentation.working);
    assert!(!presentation.show_error);
    assert_eq!(presentation.status_text, "Working...");
}

#[test]
fn error_keeps_input_available_and_exposes_message() {
    let mut state = ComposerState::new();
    state.set_status(ComposerStatus::Error, "Provider unavailable");
    let presentation = state.presentation();
    assert!(!presentation.working);
    assert!(presentation.show_error);
    assert_eq!(presentation.status_text, "Provider unavailable");
}

#[test]
fn plan_visibility_is_independent_and_requires_relevant_plan() {
    let mut state = ComposerState::new();
    state.set_focused(true);
    state.set_plan_relevant(true);
    assert!(state.presentation().show_plan);
    state.set_focused(false);
    state.set_has_text(false);
    assert!(!state.presentation().show_plan);
}
```

The final test calls `set_plan_relevant(true)`; define that method in this task so later app wiring has an explicit interface.

- [ ] **Step 2: Run the focused test to verify it fails**

Run:

```bash
cargo test -p mypi-gui panels::chat::composer::tests -- --nocapture
```

Expected: FAIL because `composer.rs` and its state types do not exist yet.

- [ ] **Step 3: Implement the minimal pure state model**

Create the module with these types and behavior:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerStatus {
    Ready,
    Working,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerPresentation {
    pub expanded: bool,
    pub show_model: bool,
    pub show_plan: bool,
    pub working: bool,
    pub show_error: bool,
    pub status_text: String,
}

#[derive(Clone, Debug)]
pub struct ComposerState {
    status: ComposerStatus,
    status_text: String,
    focused: bool,
    has_text: bool,
    plan_relevant: bool,
}
```

Set `expanded = focused || has_text || working || show_error`. Set `show_model = expanded && !working`. Set `show_plan = expanded && plan_relevant && !working`. Set `working = status == ComposerStatus::Working`. Set `show_error = status == ComposerStatus::Error`. `set_status(Ready, _)` clears `status_text`; `set_status(Working, message)` stores the message; `set_status(Error, message)` stores the message. Register the module from `panels/chat/mod.rs` with `mod composer; pub use composer::{...};` following the existing module style.

- [ ] **Step 4: Run the focused tests to verify they pass**

Run:

```bash
cargo test -p mypi-gui panels::chat::composer::tests -- --nocapture
```

Expected: PASS for all composer presentation tests.

- [ ] **Step 5: Commit the state boundary**

```bash
git add crates/mypi-gui/src/panels/chat/composer.rs crates/mypi-gui/src/panels/chat/mod.rs
git commit -m "feat: add composer presentation state"
```

---

### Task 2: Add shared warm-studio visual primitives

**Files:**
- Modify: `crates/mypi-gui/src/components/primitives.rs`

**Interfaces:**
- Produces reusable Makepad components `WarmComposerSurface`, `WarmComposerChip`, and `WarmComposerAction` for `app/mod.rs`.
- Components must remain styling primitives; they must not own app state or event behavior.

- [ ] **Step 1: Add the primitive definitions**

Extend the existing `script_mod!` with these components, preserving the existing primitives:

```rust
mod.components.WarmComposerSurface = RoundedView {
    width: Fill
    height: Fit
    draw_bg +: {
        color: #x29231f
        border_color: #x51443d
        border_size: 1.0
        border_radius: 11.0
    }
}

mod.components.WarmComposerChip = Button {
    width: Fit
    height: 24
    padding: Inset{left: 9 right: 9 top: 2 bottom: 2}
    draw_bg +: {
        color: #x332a25
        color_hover: #x46332a
        color_down: #x59402c
        border_color: #x604a3f
        border_color_hover: #xa86a4c
        border_size: 1.0
        border_radius: 6.0
    }
    draw_text +: {
        color: #xd8c0ad
        color_hover: #xf0d7c1
        color_down: #xffe4ca
        text_style +: { font_size: 9.0 }
    }
}

mod.components.WarmComposerAction = Button {
    width: Fit
    height: 28
    padding: Inset{left: 11 right: 11 top: 2 bottom: 2}
    draw_bg +: {
        color: #xb96543
        color_hover: #xd17b50
        color_down: #e39a5d
        border_radius: 7.0
    }
    draw_text +: {
        color: #xfff5ea
        text_style: theme.font_bold { font_size: 9.5 }
    }
}
```

Use the project’s existing Makepad syntax and adjust only syntax required by compilation. Keep the color values centralized here; the app layout should reference these components instead of duplicating their base styling.

- [ ] **Step 2: Run GUI compilation**

Run:

```bash
cargo check -p mypi-gui
```

Expected: PASS. If Makepad reports a DSL property incompatibility, retain the same visual values using the closest existing `RoundedView`/`Button` property syntax already used in the file.

- [ ] **Step 3: Commit shared primitives**

```bash
git add crates/mypi-gui/src/components/primitives.rs
git commit -m "style: add warm composer primitives"
```

---

### Task 3: Restyle the composer shell and adaptive controls

**Files:**
- Modify: `crates/mypi-gui/src/app/mod.rs` around the `input_bar`, `prompt_input`, `composer_footer`, model picker, and send button declarations.

**Interfaces:**
- Consumes `ComposerPresentation` from Task 1 and warm components from Task 2.
- Produces stable widget IDs used by `App::apply_composer_presentation`: `input_bar`, `composer_footer`, `composer_status`, `composer_hint`, `plan_toggle_btn`, `model_picker`, `send_btn`, and `stop_btn`.

- [ ] **Step 1: Replace the current cool-gray composer styling**

Change `input_bar` to use warm charcoal values and a larger radius while retaining its existing flow and text input. Use the following target values:

```text
idle surface: #x29231f
resting border: #x51443d
focused border: #xb96543
working accent: #xd49a52
error border: #xb85c55
text: #xffeee2
placeholder: #x9f8879
hint: #xb39a88
```

Keep the existing multiline bounds (`min 56`, `max 180`) and `submit_on_enter: true`; do not change input semantics.

- [ ] **Step 2: Add explicit status and stop widgets**

Inside `composer_footer`, add a hidden status row/label and a hidden `stop_btn`. The status row must include text, not only color/animation:

```text
composer_status := Label { visible: false text: "" }
stop_btn := Button { visible: false text: "Stop" }
```

Style `stop_btn` with amber/coral attention treatment and preserve the existing send button’s action path by routing the new stop action to the current cancellation behavior.

- [ ] **Step 3: Make model and plan controls adaptive**

Keep the existing model picker and plan toggle IDs. Set their default visibility to false for the idle layout. `App::apply_composer_presentation` will reveal them only when presentation says they are relevant. Preserve the existing labels and click behavior.

- [ ] **Step 4: Update the send action treatment**

Replace the current blue/glass-only send presentation with the warm terracotta action styling while preserving `send_btn` ID and click handling. Use `↑` for ready/send and `Stop` or `■ Stop` for working, but ensure the working state has a textual label.

- [ ] **Step 5: Compile the declarative layout**

Run:

```bash
cargo check -p mypi-gui
```

Expected: PASS, with all widget IDs referenced by the existing Rust code still present.

- [ ] **Step 6: Commit the layout styling**

```bash
git add crates/mypi-gui/src/app/mod.rs
git commit -m "style: refresh composer warm adaptive layout"
```

---

### Task 4: Wire presentation state to existing app status and input events

**Files:**
- Modify: `crates/mypi-gui/src/app/mod.rs`

**Interfaces:**
- Consumes `ComposerState`, `ComposerStatus`, and `ComposerPresentation` from `crate::panels::chat`.
- Produces `App::apply_composer_presentation(&mut self, cx: &mut Cx)` and uses it from status/input transitions.

- [ ] **Step 1: Add composer state to `App`**

Add a Rust field beside `busy`:

```rust
#[rust]
composer_state: ComposerState,
```

Initialize it with `ComposerState::new()` wherever `App` has explicit Rust initialization/default setup. If Makepad’s generated initialization requires `#[rust]` default construction, derive/implement `Default` for `ComposerState` by returning `Self::new()`.

- [ ] **Step 2: Implement widget application**

Add this method near `set_status`:

```rust
fn apply_composer_presentation(&mut self, cx: &mut Cx) {
    let presentation = self.composer_state.presentation();
    self.ui.widget(cx, ids!(input_bar)).apply_over(cx, live! {
        draw_bg: {
            border_color: #x51443d
        }
    });
    self.ui.widget(cx, ids!(composer_status)).set_visible(cx, presentation.working || presentation.show_error);
    self.ui.label(cx, ids!(composer_status)).set_text(cx, &presentation.status_text);
    self.ui.button(cx, ids!(model_picker)).set_visible(cx, presentation.show_model);
    self.ui.button(cx, ids!(plan_toggle_btn)).set_visible(cx, presentation.show_plan);
    self.ui.button(cx, ids!(send_btn)).set_visible(cx, !presentation.working);
    self.ui.button(cx, ids!(stop_btn)).set_visible(cx, presentation.working);
    self.ui.widget(cx, ids!(input_bar)).redraw(cx);
}
```

Adapt the exact Makepad accessor used for `model_picker` to the existing widget type (the current code accesses its child button as `model_picker_btn`). For focus/error borders, use the project’s established `script_apply_eval!`/`apply_over` pattern and keep the base colors in the DSL. Do not create a second source of truth for `busy`.

- [ ] **Step 3: Update `set_status` without changing agent semantics**

At the start of `set_status`, map the existing `UiStatus` to `ComposerStatus`, call `composer_state.set_status`, retain the existing `busy` assignment/session spinner updates, and call `apply_composer_presentation(cx)` after all visibility changes. Preserve all current callers and status text inputs; the `_text` parameter must become the displayed status message.

- [ ] **Step 4: Track focus and typing through existing input actions**

In the existing `prompt_input` event handling, detect the text value after the current input action handling and call:

```rust
self.composer_state.set_has_text(!draft.trim().is_empty());
self.composer_state.set_focused(/* true when the input owns keyboard focus */);
self.apply_composer_presentation(cx);
```

Use Makepad’s existing focus/action information in the file; do not poll on a timer. If the current widget API cannot expose focus cleanly, use the input’s focus-in/focus-out actions and document that the typing signal alone controls expansion. Preserve command popup focus behavior.

- [ ] **Step 5: Connect plan relevance**

In `refresh_plan_ui`, after computing `enabled`, call `composer_state.set_plan_relevant(enabled)` and then `apply_composer_presentation(cx)`. Do not remove the existing plan drawer visibility logic; the composer chip and drawer remain separate.

- [ ] **Step 6: Run focused and compile checks**

Run:

```bash
cargo test -p mypi-gui panels::chat::composer::tests -- --nocapture
cargo check -p mypi-gui
```

Expected: all composer state tests PASS and GUI compilation PASS.

- [ ] **Step 7: Commit app wiring**

```bash
git add crates/mypi-gui/src/app/mod.rs
git commit -m "feat: wire adaptive composer states"
```

---

### Task 5: Apply warm styling to the command popup

**Files:**
- Modify: `crates/mypi-gui/src/panels/command_palette/view.rs`

**Interfaces:**
- Consumes the existing `items`, keyboard focus index, pointer hover index, and popup visibility behavior.
- Produces no behavior/API changes; only visual treatment changes.

- [ ] **Step 1: Restyle popup container**

Change the popup base from cool blue-gray to the composer warm surface:

```text
surface: #x29231f
border: #x604a3f
focused/active border: #xb96543
radius: 9
```

Keep the existing custom SDF border implementation and change its color and radius inputs to the warm values above. Keep popup width, list height, filtering, and scrolling unchanged.

- [ ] **Step 2: Restyle command rows and selection**

Use warm text hierarchy:

```text
command name: #xffeee2
command description: #xb39a88
row hover: #x46332a
row active: #x59402c
active marker: #xd17b50
```

Keep the current `keyboard_focus_index` and `pointer_hover_index` logic. Do not alter which item is selected or how Enter/click invokes it.

- [ ] **Step 3: Run command palette tests and compile**

Run:

```bash
cargo test -p mypi-gui panels::command_palette -- --nocapture
cargo check -p mypi-gui
```

Expected: existing command palette tests PASS and GUI compilation PASS.

- [ ] **Step 4: Commit popup styling**

```bash
git add crates/mypi-gui/src/panels/command_palette/view.rs
 git commit -m "style: warm command palette"
```

---

### Task 6: Verify behavior, responsive layout, and regression safety

**Files:**
- Modify: `crates/mypi-gui/src/panels/chat/composer.rs` only if test corrections are required by actual state behavior.
- Modify: `crates/mypi-gui/src/app/mod.rs` only for verified layout/state defects found during validation.

**Interfaces:**
- Validates the complete composer behavior from Tasks 1–5; no new public interfaces.

- [ ] **Step 1: Run all GUI tests**

Run:

```bash
cargo test -p mypi-gui -- --nocapture
```

Expected: PASS for chat, command palette, workspace, and composer tests.

- [ ] **Step 2: Run formatting and static checks**

Run:

```bash
cargo fmt --all -- --check
cargo clippy -p mypi-gui --all-targets -- -D warnings
```

Expected: no formatting differences and no new warnings.

- [ ] **Step 3: Run the workspace test suite**

Run:

```bash
cargo test --workspace
```

Expected: PASS. Existing unrelated failures must be reported rather than hidden or broadly fixed in this UI plan.

- [ ] **Step 4: Perform a manual state checklist**

Launch the GUI with `cargo run -p mypi-gui` using the repository’s normal environment and check at minimum:

1. Idle composer is compact, warm, and shows the hint without a large empty toolbar.
2. Focusing or typing expands the composer and reveals the model control.
3. Enter sends and Shift+Enter inserts a newline.
4. `/` opens the attached warm command popup; arrow keys and Enter still work.
5. Relevant plan state reveals the plan chip without forcing the plan drawer open.
6. Working state shows an amber indicator and explicit Stop action.
7. Stop returns to ready without breaking the input.
8. Provider/agent error retains typed input and shows muted coral status.
9. A subsequent request returns the composer to normal styling.
10. The composer remains usable at both the default 1280×768 window and a narrow resized window; no controls are clipped or inaccessible.

- [ ] **Step 5: Inspect the final diff for scope**

Run:

```bash
git diff --stat HEAD~5..HEAD
git status --short
```

Confirm that implementation commits touch only the planned GUI files and that unrelated pre-existing worktree changes were not staged.

- [ ] **Step 6: Commit verified fixes, if any**

Only if Task 6 found and fixed a concrete defect:

```bash
git add crates/mypi-gui/src/panels/chat/composer.rs crates/mypi-gui/src/app/mod.rs
git commit -m "fix: polish composer state transitions"
```

---

## Self-Review Coverage

- Visual language: Task 2 centralizes warm primitives; Tasks 3 and 5 apply the palette.
- Idle/focused/typing states: Tasks 1, 3, and 4.
- Slash-command mode: Task 5, with behavior regression checks in Tasks 5–6.
- Working and stop state: Tasks 3–4.
- Error state and recovery: Tasks 1 and 4.
- Adaptive control hierarchy: Tasks 1, 3, and 4.
- Existing behavior preservation: Tasks 3–6 retain IDs and run focused/workspace tests.
- Responsive verification: Task 6 manual checklist.
- No new dependencies/API/persistence: File map and global constraints.

The plan contains no placeholders or unassigned requirements. The `ComposerState` method names and widget IDs are defined before later tasks consume them.
