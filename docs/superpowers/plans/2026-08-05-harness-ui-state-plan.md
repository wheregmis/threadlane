# Harness UI State Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the live V2 harness activity, queue, tool replay, usage, and fault state in the existing task sidebar and chat activity rail.

**Architecture:** Add a small display-only summary derived from the existing harness snapshot/reducer state. Keep persistence and execution unchanged; update the existing `HarnessActivity`/`TaskSidebarItem` presentation data and render compact primary text with expandable detail.

**Tech Stack:** Rust, Makepad widgets, existing `threadlane-agent::harness::Snapshot`, existing chat/task-sidebar tests.

## Global Constraints

- Reuse the existing V2 snapshot/event path; do not add a second runtime status registry.
- Keep raw journal records out of the UI.
- Preserve the existing dark visual language and compact sidebar geometry.
- Run focused tests, `cargo fmt --check`, `cargo check -p threadlane`, and `git diff --check`.

---

### Task 1: Add display summary data and tests

**Files:**
- Modify: `crates/threadlane/src/panels/chat/state.rs`
- Modify: `crates/threadlane/src/app/mod.rs`
- Modify: `crates/threadlane/src/components/task_sidebar.rs`

**Interfaces:**
- `HarnessActivity` gains a bounded display detail assembled from current V2 state.
- Existing sidebar rows continue consuming `TaskSidebarItem`; only its `activity`/detail text becomes richer.

- [x] **Step 1: Write failing tests** for queue labels, active tool/replay labels, usage, and storage-fault text using the existing pure state helpers.
- [x] **Step 2: Run the focused app tests** and confirm the new assertions fail.
- [x] **Step 3: Implement the smallest formatter and snapshot mapping** that produces compact strings such as `Running tests · 2 queued · tool: shell · replay-safe` and `Storage fault · ...`.
- [x] **Step 4: Update the chat rail and task sidebar** so the compact activity is visible in the row and the existing details expansion shows the bounded full summary.
- [x] **Step 5: Run focused tests and inspect the diff.**

### Task 2: Verify runtime behavior

**Files:**
- No new production files.

- [x] **Step 1:** Run `cargo fmt` and `cargo fmt --check`.
- [x] **Step 2:** Run `cargo check -p threadlane`.
- [x] **Step 3:** Run `git diff --check`.
- [ ] **Step 4:** Launch a fresh Makepad Studio build and verify queued, running, tool execution, recovery, abort, and completion states in the sidebar/chat rail.
