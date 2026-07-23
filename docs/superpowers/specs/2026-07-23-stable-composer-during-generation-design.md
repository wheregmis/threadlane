# Stable Composer During Generation

## Goal

Keep the chat composer visually stable while a task is running. The existing chat activity indicator already communicates progress, so the composer must not duplicate that state with a `Working...` label or rearrange its controls.

## User Experience

The ready and working composer states use the same dimensions, spacing, and control positions. When generation starts, only the rightmost action changes:

- Ready: show the Send action.
- Working with an active generation: show the Stop action in the Send action's position.
- Working without a cancellable generation: do not show Stop; the rest of the composer remains unchanged.

The attachment action, keyboard hint, thinking picker, model picker, prompt input, and attachment row retain their normal layout and visibility while generation is active. The chat activity indicator remains the sole working-state indicator.

## Implementation Boundary

Update the existing composer presentation state and `apply_composer_presentation` wiring rather than introducing another component or overlay. Remove the composer status label from the layout and stop updating it. Preserve the existing separate Send and Stop widgets and cancellation behavior; visibility determines which action occupies the final footer slot.

Errors continue through the application's existing status and chat feedback. They must not introduce a composer status label or change composer geometry.

## State Flow

1. Session status changes to working.
2. The chat activity indicator becomes visible.
3. Normal composer controls remain visible.
4. Send becomes hidden and Stop becomes visible when the active session has a cancellable generation.
5. On completion, cancellation, or error, Stop becomes hidden and Send returns without any other composer layout change.

## Testing

Update composer presentation unit tests to verify:

- Ready state keeps normal controls available.
- Working state keeps normal controls available rather than hiding model-related controls.
- Working state does not expose composer status text.
- Working state selects the Stop action while Ready and Error select Send.
- Error state does not reshape the composer.

Run the focused `threadlane-gui` tests and the repository's relevant formatting/check commands.

## Non-goals

- Changing the chat activity indicator.
- Redesigning Send or Stop styling.
- Changing cancellation semantics or draft restoration.
- Allowing a second prompt to be submitted while generation is active.
