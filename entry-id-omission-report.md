# Entry-ID Omission Fix

## Root cause

Checkpoint omission matched `source_tool_call_id`, but call IDs are not globally unique.
An indexed read result therefore hid any later non-indexed tool result sharing that call ID.

## Fix

Checkpoint construction now receives compacted durable entries and replaces only entries whose
IDs match indexed snapshots' `source_entry_id` values.

## Regression coverage

`compacted_context_snapshot_stays_durable_and_is_indexed_in_checkpoint` adds a later durable
tool-result entry with the indexed result's call ID. It verifies the indexed read body is omitted
while the later result remains in the checkpoint. The assertion failed before the fix and passes
after it.

## Verification

- `cargo test -p threadlane-session compacted_context_snapshot_stays_durable_and_is_indexed_in_checkpoint`
- `cargo test -p threadlane-runtime compaction`
- `cargo test -p threadlane-session coding_agent::harness::tests`
- `cargo fmt`
- `git diff --check`
