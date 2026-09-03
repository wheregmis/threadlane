# Structured Index Fix Report

## Root causes

- Adaptive/manual checkpoint text was built from every compacted tool result, so an indexed `read_file` body was duplicated into provider-visible summary text even though its durable snapshot metadata was available separately.
- Manual compaction crossed `durable.rs` into `CompactionProcedure::accept` with only a summary string; the procedure therefore persisted `context_snapshot_index: []` instead of the same bounded structured metadata used by adaptive compaction.

## Fixes

- Checkpoint construction now replaces only indexed tool-result bodies with a short snapshot placeholder. Non-indexed context is still summarized normally, and the raw indexed entry remains durable for `manage_context` reloads.
- Manual compaction recomputes the bounded structured index with `compacted_context_snapshot_index`, carries it through `AgentHarness` and `CompactionProcedure`, and leaves the supplied summary string unchanged.

## Regression coverage

- `compacted_context_snapshot_stays_durable_and_is_indexed_in_checkpoint` now uses the normal checkpoint cap, proves provider-visible text excludes the raw indexed body, and proves non-indexed evidence survives.
- `manual_compaction_preserves_summary_and_structured_snapshot_index` exercises the durable manual-compaction boundary, verifies exact summary preservation, and verifies structured snapshot metadata without body duplication.

Both tests were observed failing for their intended regressions before the production changes, then passing afterward.

## Verification

- `cargo test -p threadlane-runtime` — 109 passed, 2 ignored.
- `cargo test -p threadlane-session` — 170 passed.
- `cargo check -p threadlane-gpui` — passed with 6 pre-existing unused-code warnings.
- `cargo fmt --all -- --check` — passed.

## Tokensave index note

The worktree's `.tokensave/tokensave.db` contains three indexed non-Rust files and zero code nodes, so it could not answer the requested compaction call-flow query. Repository search was used as the documented fallback. If reporting this limitation at <https://github.com/aovestdipaperino/tokensave>, strip any sensitive or proprietary code from the issue description.
