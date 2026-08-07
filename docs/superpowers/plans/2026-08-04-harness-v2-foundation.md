# Harness V2 Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish the compatibility-preserving durable store and pure reducer required by Harness V2.

**Architecture:** Add a focused `threadlane-agent::harness` module. Keep existing `SessionTree` and `OpRecord` as compatibility inputs, use an in-memory backend as the semantic reference, and make reduction pure and deterministic.

**Tech Stack:** Rust 2021, serde/serde_json, existing JSONL session persistence, Cargo unit/integration tests.

## Global Constraints

- Existing Threadlane JSONL sessions open as idle `main` lanes.
- No provider, tool, hook, timer, or synthetic effect occurs during reduction.
- Malformed complete records fail; only a torn final JSONL line is tolerated.
- Do not change foreground execution in the foundation slice.
- Do not add a dependency until the existing workspace dependencies cannot solve the requirement.

### Task 1: Define durable harness types

**Files:**
- Create: `crates/threadlane-agent/src/harness/types.rs`
- Create: `crates/threadlane-agent/src/harness/mod.rs`
- Modify: `crates/threadlane-agent/src/lib.rs`
- Test: `crates/threadlane-agent/src/harness/types.rs`

- [ ] Add typed IDs, entries, lanes, durable records, corruption errors, and reduced lane state with serde support.
- [ ] Export only the landed foundation surface.
- [ ] Add round-trip tests for each record family and idle legacy state.

### Task 2: Add memory store and strict reducer

**Files:**
- Create: `crates/threadlane-agent/src/harness/memory.rs`
- Create: `crates/threadlane-agent/src/harness/reducer.rs`
- Test: `crates/threadlane-agent/tests/harness_recovery.rs`

- [ ] Implement append/query operations and monotonic session sequencing in `MemoryStore`.
- [ ] Reduce records into idle, suspended, or terminal lane state without effects.
- [ ] Reject duplicate IDs, missing parents, invalid lanes, decreasing sequences, and invalid record relationships.
- [ ] Assert reduction is fixed-point and invalid prefixes return specific errors.

### Task 3: Add JSONL compatibility adapter

**Files:**
- Create: `crates/threadlane-agent/src/harness/jsonl.rs`
- Test: `crates/threadlane-agent/tests/harness_compat.rs`

- [ ] Load legacy sessions through `SessionTree` and expose their active branch/configuration unchanged.
- [ ] Load operation sidecars through existing compatibility decoding.
- [ ] Validate complete malformed lines and tolerate only a torn final line.
- [ ] Add fixtures for messages, metadata, facts, plans, images, passive branches, and torn tails.

### Task 4: Verify and iterate

**Files:**
- No additional production files unless a test exposes a contract gap.

- [ ] Run focused harness tests.
- [ ] Run `cargo test -p threadlane-agent`.
- [ ] Run `cargo check -p threadlane` and `git diff --check`.
- [ ] Re-read `harness_v2.md` and record the next uncovered milestone before continuing.
