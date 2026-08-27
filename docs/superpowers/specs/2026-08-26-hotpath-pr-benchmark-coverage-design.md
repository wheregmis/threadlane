# Hotpath PR Benchmark Coverage

## Goal

Every pull request reports comparable performance changes for the repository's deterministic, headless hot paths. The suite should expose regressions without requiring UI automation, network access, or a new benchmark framework.

## Scope

Keep four Hotpath suite binaries in the non-published `crates/threadlane-benchmarks` crate:

- `runtime`: fixed-cardinality JSONL append/open, reducer replay, and session reload.
- `tools`: warm in-process repository search.
- `mcp`: warmed discovery/reconnect and steady-state tool calls against a local stub server.
- `terminal`: VT100 parsing and resize/scrollback work without launching GPUI.

Benchmarks use fixed realistic workloads and `std::hint::black_box` where needed. Filesystem and subprocess setup occurs outside measured functions whenever Hotpath permits it. Existing production helpers are reused; benchmark-only public APIs are not added unless no narrower option exists.

## CI and Reporting

`hotpath-profile.yml` runs timing jobs for all four suites and allocation jobs for runtime, tools, and terminal. Each entry runs the same centralized binary on the PR head and base SHA and uploads uniquely named JSON plus PR, revision, toolchain, runner, and CPU metadata. MCP remains timing-only because parent-process allocation data does not represent its child process.

`hotpath-comment.yml` verifies artifact lineage, renders each complete pair through `hotpath-utils profile-pr --dry-run`, and updates one sticky Threadlane report with compact collapsible sections. Raw metrics remain downloadable for 14 days.

A base branch that lacks a newly added suite is skipped for that suite. Other suites still report. Benchmark command failures fail profiling rather than silently publishing incomplete measurements.

## Validation

- Run every centralized benchmark binary locally in release mode with Hotpath enabled.
- Parse the emitted JSON through `hotpath-utils profile-pr --dry-run` using a file as both base and head.
- Run focused checks for every touched crate.
- Run `git diff --check`.

## Deliberate Limits

The first version excludes full GPUI rendering, real provider/network calls, and cold machine startup. Those measurements are environment-sensitive and would make per-PR comparisons noisy. Add them only when a stable headless harness and controlled runner exist.
