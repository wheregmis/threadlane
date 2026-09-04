# Final fix pass 2 report

## Findings fixed

1. **Repeated compaction duplicated the snapshot index.** `commit_prepared_compaction` appended a new `## Available context snapshots` section to a checkpoint that could already contain an older generated section. It now removes every prior generated heading and its snapshot bullet rows before appending the current bounded index. The regression runs two compactions, deliberately lets the second summarize the first checkpoint, and proves the result has exactly one index whose suffix remains within the existing 4,000-character limit.
2. **Fuzzy reads indexed the requested path instead of the file read.** Snapshot capture reconstructed the path from durable tool intent, so a successful fuzzy read of `src/view.rs` could persist that nonexistent shorthand instead of `crates/app/src/view.rs`. Local `read_file` results now carry their workspace-relative canonical path beside the existing execution-bound digest marker. Indexing requires that marker, validates it against the workspace, and persists the resolved relative path. The regression executes a fuzzy read, checks the durable path, and successfully reloads the snapshot.

## TDD evidence

- `context_snapshot_capture_persists_fuzzy_read_actual_path` failed with `left: "src/view.rs"`, `right: "crates/app/src/view.rs"` before the fix.
- `compacted_context_snapshot_stays_durable_and_is_indexed_in_checkpoint` failed after the second compaction was configured to retain the prior checkpoint, exposing the duplicate generated section.
- Both targeted regressions passed after the minimal production changes.

## Verification

- `cargo test -p threadlane-tools -- --nocapture` — 53 passed, 1 ignored.
- `cargo test -p threadlane-session --features test-support -- --nocapture` — 189 passed across 4 suites.
- `cargo check -p threadlane-gpui` — exit 0 with the repository's 6 existing unused-code warnings.
- `cargo fmt --all -- --check` — exit 0.
- `git diff --check` — exit 0.

Existing unrelated formatting-only working-tree changes and the untracked `rust_out` file were preserved and excluded from this fix commit.
