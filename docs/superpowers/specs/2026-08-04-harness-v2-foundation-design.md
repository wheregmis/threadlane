# Harness V2 Foundation Design

## Goal

Introduce the durable harness core under `threadlane-agent` without changing
foreground execution yet. The first slice covers legacy compatibility, typed
durable state, a memory reference store, strict reduction, and a JSONL adapter.

## Design

`harness::types` defines append-only entries, lane records, operation records,
and reduced lane state. `MemoryStore` is the reference implementation used by
conformance and recovery tests. `JsonlStore` initially reads existing
`SessionTree` JSONL and operation sidecars through the current loaders, then
owns new V2 writes behind one store API. `reducer` is pure: opening a session
only validates and reduces durable data, returning idle or suspended state;
it never performs provider, tool, hook, timer, or synthetic transcript work.

Legacy sessions without V2 records are exposed as one idle `main` lane with
the existing active branch, metadata, plan, facts, and transcript unchanged.
Malformed complete records are errors; only a provably torn final JSONL line
may be ignored.

## Boundaries

No foreground loop, UI, provider, SQLite backend, ACP path, or specialized
subagent recovery path changes in this slice. Later milestones may replace
the adapter internals while retaining this public store/reducer contract.

## Verification

Add focused tests for legacy loading, strict validation, deterministic
reduction, duplicate IDs, missing parents, sequence ordering, and repeated
reduction. Run `cargo test -p threadlane-agent` and `git diff --check`.
