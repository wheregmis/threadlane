# Ultimate Context Snapshot Fix Report

## Important findings fixed

1. **Indexed-output binding** — snapshot loads now require the deterministic context/source-entry relationship, the matching durable `ToolStarted` provenance and line range, and source-output digest/path markers that exactly match the index. Relabelled or forged records return `corrupt` without exposing the referenced body.
2. **Compaction selection and discovery** — snapshots whose source entries leave model context in the current compaction are selected before the global newest-first cap. `manage_context list` now accepts `before_context_id`, so sessions with more than 20 snapshots can page to older loadable IDs.
3. **Queued child freshness** — subagent context references are resolved only after the child acquires its concurrency permit, immediately before lane persistence and provider work. A file changed while the child waits is rejected as stale.
4. **Complete trajectory telemetry** — every snapshot index now emits a typed capture item, every load record is emitted even when its context ID is unknown, and duplicate physical reads compare `(source lane, path, range, digest)` against all earlier captures. `A -> B -> A` therefore marks the final `A` as a duplicate candidate.
5. **Concurrent load durability** — load-record IDs use a process/timestamp/atomic nonce instead of a pre-append sequence number. Successful loads propagate telemetry persistence failures rather than returning content as though recording succeeded.

## Regression evidence

The new tests were observed failing before their corresponding production changes:

- forged context IDs and forged path/digest metadata returned the original source body; a forged range was also accepted;
- the bounded index omitted a source leaving context when 20 newer snapshots existed, and `before_context_id` was rejected;
- a child queued behind the semaphore accepted content that changed while it waited;
- trajectory projection had no capture variant, omitted unknown loads, and only compared the immediately previous digest;
- 32 concurrent successful loads produced only 7 distinct durable load records while all callers reported success.

The focused regressions then passed after the fixes, including a mutation check proving the harness-level compaction test fails when dropped-source priority is removed.

## Verification

- `cargo test -p threadlane-runtime -- --nocapture` — 109 passed, 2 ignored.
- `cargo test -p threadlane-session --features test-support -- --nocapture` — 198 passed across 4 suites.
- `cargo check -p threadlane-gpui` — passed with the repository's 6 existing unused-code warnings.
- `cargo fmt --all -- --check` — passed.
- `git diff --check` — passed.

## Scope notes

No dependency, datastore, provider path, ACP behavior, UI, snapshot body copy, or automatic read suppression was added. Pre-existing unrelated dirty formatting changes and the untracked `rust_out` file were preserved and excluded from this fix.

The worktree's `.tokensave/tokensave.db` contains zero indexed nodes, so repository search was required after the documented tokensave-first attempt. If reporting that limitation at <https://github.com/aovestdipaperino/tokensave>, strip all sensitive or proprietary code from the issue description.

Commit subject: `fix(context): close snapshot review gaps`.
