# Context snapshot index marker fix

## Change

- Added exact begin/end sentinels around generated compacted context indexes.
- Stripping now removes only complete sentinel-delimited indexes, preserving ordinary summary headings and bullets.
- Reserved sentinel space within the existing 4,000-character cap; the existing 20-snapshot limit remains unchanged.

## Regression coverage

- `compacted_context_index_stripping_preserves_user_heading_and_bullets`
- `compacted_context_snapshot_stays_durable_and_is_indexed_in_checkpoint`
- `manage_context_lists_the_twenty_newest_snapshots`

## Verification

- `cargo test -p threadlane-session` — 169 passed.
- `cargo check -p threadlane-gpui` — passed (6 pre-existing unused-code warnings).
- `cargo fmt --check` and `git diff --check` — passed.
