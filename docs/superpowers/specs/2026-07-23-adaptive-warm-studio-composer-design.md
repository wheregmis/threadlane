# Adaptive Warm-Studio Prompt Composer Design

**Date:** 2026-07-23  
**Status:** Approved

## Goal

Improve the mypi prompt composer visually and behaviorally without changing the underlying agent workflow: keep it quiet when idle, reveal useful controls while focused, make command mode feel attached to the composer, and communicate working/error states clearly.

## Scope

This phase is limited to the prompt composer and its slash-command popup. It includes visual tokens, layout states, control visibility, and presentation-state wiring needed to render those states. It does not add new agent capabilities, context attachment, permissions management, token counters, or a new composer widget architecture.

Existing text input, Enter/Shift+Enter behavior, command completion, model selection, plan controls, and stop behavior must remain functional.

## Visual Direction

The composer adopts a warm-studio palette while retaining a dark developer-tool foundation:

- Deep warm charcoal for the idle composer surface.
- Slightly lifted brown-charcoal for focused/expanded presentation.
- Muted warm gray for the resting border.
- Terracotta for the primary action and focused border.
- Amber for active/attention states, including working status and hover emphasis.
- Warm off-white for entered text.
- Soft tan-gray for placeholder and hint text.
- Muted coral-red for errors, visually distinct from terracotta.

Shared colors should be centralized in reusable Makepad primitives where practical. New one-off colors should be avoided.

## Composer States

### Idle

The composer is a compact, single-line dock with a subtle hint row. The input and send affordance remain available. Model and plan controls are minimized or hidden unless relevant. The surface should not reserve a large empty toolbar area.

### Focused or Typing

The composer expands into a comfortable card with increased input breathing room. The model chip becomes visible, and the plan chip appears only when plan state is relevant. The send action uses the terracotta primary treatment. The keyboard hint remains visible but subordinate.

The exact focus/typing signal must follow existing Makepad event and widget patterns rather than introducing fragile polling.

### Slash-Command Mode

The command popup is visually attached to the composer and uses the same warm surface, border, radius, and typography system. The active keyboard selection uses a warm highlight and terracotta marker. Command names have stronger hierarchy than descriptions. Existing keyboard navigation, filtering, and selection behavior remain unchanged.

### Working

While the agent is working, the composer communicates the state with an amber activity indicator and a clear working label. The send action becomes an explicit stop action, including text or an unambiguous icon plus accessible visual distinction. Existing stop behavior is preserved. The input is visually subdued or disabled according to current behavior.

### Error

Errors use a muted coral border/status treatment and a concise explanatory status line. The input remains available for correction and retry. Error presentation must clear or return to normal when the next request begins or the error is otherwise resolved.

## Control Hierarchy

Controls are adaptive rather than permanently displayed:

- Always available: prompt input and send/stop action.
- Visible on focus or typing: model selector and relevant plan state.
- Existing surrounding UI or future overflow surface: context/files, permissions, and advanced settings.
- Deferred: token/context counters and new attachment systems.

This keeps the composer capable without turning it into a persistent settings toolbar.

## Implementation Boundaries

The current `MypiCommandTextInput` remains responsible for text editing and command completion. It should be styled and coordinated, not duplicated or replaced.

Expected files:

- `crates/mypi-gui/src/app/mod.rs`: composer layout, warm styling, control visibility, and working/error presentation wiring.
- `crates/mypi-gui/src/components/primitives.rs`: shared warm-studio surface/button/chip styling if reusable primitives are needed.
- `crates/mypi-gui/src/panels/command_palette/view.rs`: command popup styling and selection treatment.
- `crates/mypi-gui/src/panels/chat/state.rs`: only if a small explicit presentation-state model is needed; no agent behavior belongs here.

Do not extract a new composer widget during this phase.

## Data and Event Flow

Existing app state remains the source of truth for whether the agent is ready, working, or failed. Composer presentation updates must be derived from existing widget actions and app status transitions. Text input and command-palette actions continue through their current event path. Styling and visibility changes should trigger the normal Makepad redraw path.

No persistence or API changes are required.

## Error Handling

- Failed requests retain the user’s input and show the composer error state.
- Starting a new request clears the prior error presentation.
- A stop action returns the composer to a ready/idle or ready/focused state without losing typed text unless existing behavior explicitly clears it.
- Command popup data errors must not prevent the base composer from rendering.

## Verification

Verification must cover:

1. Idle compact layout and warm styling.
2. Focused and typing expansion.
3. Existing Enter-to-send and Shift+Enter newline behavior.
4. Slash-command popup attachment, filtering, keyboard navigation, and selection.
5. Model picker behavior and visibility.
6. Plan control visibility and interaction.
7. Working indicator and stop behavior.
8. Error presentation, retry, and recovery.
9. Narrow and wide window layouts without clipping or inaccessible controls.
10. Existing workspace tests and GUI compilation, while preserving unrelated pre-existing worktree changes.

## Non-Goals

- Redesigning the overall app shell.
- Adding task/project navigation.
- Adding new agent states or capabilities.
- Implementing context/file attachments.
- Adding permissions UI.
- Adding a package, skill, or extension manager.
- Replacing Makepad widgets or introducing a new UI framework.
