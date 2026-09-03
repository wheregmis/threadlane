# Marker parser fix

`strip_compacted_context_indexes` now removes only a complete generated index:
the begin marker must be immediately followed by the exact generated heading,
and no nested begin marker may appear before its end marker. Other input is
copied byte-for-byte, including trailing newlines.

Regression coverage includes nested markers, malformed headings, unpaired
markers, and a valid generated index. The nested regression failed before the
fix and passes now.

Verification:

- `cargo test -p threadlane-session compacted_context_index_stripping --lib`
- `cargo test -p threadlane-session --lib`
- `git diff --check`
