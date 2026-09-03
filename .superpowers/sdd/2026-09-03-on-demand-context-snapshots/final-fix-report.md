# Final review fix report

## Findings addressed

1. **Read/digest race** — `read_file` now computes a SHA-256 marker from the same in-memory UTF-8 bytes used to render its result. Snapshot indexing accepts only that execution-bound marker and never re-reads the file to establish the captured digest. A successful local result without the marker is skipped. The regression reads `contents`, changes the file before inspection/indexing, and proves the captured digest remains the digest of `contents` while resolution reports the changed file as stale.
2. **Virtual and remote paths** — capture rejects `skill:`, `agent:`, `pr:`, `mr:`, `issue:`, `virtual:`, `file:`, `http:`, and `https:` prefixes case-insensitively before workspace validation.
3. **Repeated compaction** — each compaction checkpoint renders the bounded current session snapshot projection, so a snapshot remains advertised after its source entry left model context during an earlier compaction. The production-path regression performs two compactions and checks both checkpoints.
4. **Malformed metadata recovery** — full-history reduction skips semantically invalid `ContextSnapshotIndexed` and `ContextSnapshotLoaded` records while retaining strict candidate validation for new appends. A hand-written malformed journal reloads successfully with no projected snapshot.
5. **Duplicate-read telemetry** — duplicate candidacy is derived when `ContextSnapshotIndexed` records are encountered from the latest earlier identical `(source lane, path, start line, end line, digest)` capture. Later load order and requesting/handoff lane no longer change the classification.
6. **Error and rendering polish** — stale/missing errors include the recorded location and explicitly direct the agent to call `read_file`; snapshot headers, compaction indexes, and subagent handoffs omit the old `?-?` suffix when no range was requested.

## Focused regression evidence

- TDD red evidence:
  - `threadlane-tools` digest-binding test initially failed because `read_file_snapshot_digest` did not exist.
  - malformed JSONL reload failed with `InvalidRecord("invalid context snapshot")`, then with `InvalidRecord("invalid context snapshot load")` when the load-record case was added.
  - duplicate telemetry produced `[false, true, false]` instead of capture-derived `[true, false, false]` when load order was reversed and requesting lanes varied.
  - the second compaction checkpoint omitted the context ID.
  - actual virtual schemes were accepted by the capture predicate.
  - absent ranges rendered as `README.md:?-?`.
- Green suites:
  - `cargo test -p threadlane-tools -- --nocapture` — 53 passed, 1 ignored.
  - `cargo test -p threadlane-runtime -- --nocapture` — 107 passed; doc tests 2 ignored.
  - `cargo test -p threadlane-session --features test-support -- --nocapture` — 167 unit tests and 21 ACP integration tests passed.
  - `cargo check -p threadlane-gpui` — exit 0 with the repository's 6 existing unused-code warnings.
  - `cargo fmt --all -- --check` — exit 0.
  - `git diff --check` — exit 0.

## Scope and delivery

No datastore, ACP protocol, provider payload, UI, automatic read suppression, or snapshot-body duplication was added. Existing unrelated formatting-only working-tree changes and the untracked `rust_out` file were left unstaged and unmodified by this commit.

Commit subject: `fix(context): harden durable snapshot reuse`.
