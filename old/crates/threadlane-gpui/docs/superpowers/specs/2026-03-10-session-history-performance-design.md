# Session History Performance Design

## Goal

Reduce session-history parsing and idle chat CPU without changing visible behavior.

## Scope

1. Load a session file once when selecting a session and derive both its active plan and visible message page from that tree.
2. Avoid projecting the entire message history when only the newest page is needed.
3. Cache session discovery metadata for unchanged files, while invalidating entries when files change or disappear.
4. Keep the chat stream loop responsive during generation and settling, but use slower idle pacing and avoid redundant notifications.

## Approach

Use the existing `SessionTree` and `AppState` paths rather than introducing a second persistence format or a parallel event system. Keep pagination semantics unchanged: the visible page is the newest 40 projected messages, and older pages use the existing cursor. Discovery caching is keyed by session path and file metadata. The stream loop retains fast 16 ms settling frames and uses a slower idle interval.

## Behavior and error handling

Unreadable sessions continue to appear as warning entries. Missing or modified files invalidate discovery cache entries. Session loading failures continue to produce empty message/plan state. No UI behavior or persistence format changes.

## Verification

Add focused tests for page projection and discovery cache invalidation where practical. Run `cargo check -p threadlane-gpui`, focused tests, and `git diff --check`.
