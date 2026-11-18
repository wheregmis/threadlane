# Session History Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce session-history parsing and idle chat CPU without changing visible behavior.

**Architecture:** Reuse `SessionTree` as the single source of persisted session data. Add one internal loaded-session projection for plan/history reuse, make message projection page-aware, and add a metadata-keyed discovery cache inside `AppState`. Keep stream polling event-driven in spirit: fast while settling, slower when idle, with notifications only for actual state changes.

**Tech Stack:** Rust, GPUI, `threadlane-agent::SessionTree`, existing AppState tests.

## Global Constraints

- Preserve the existing session JSONL format and visible pagination semantics.
- Do not introduce a second persistence or event path.
- Unreadable sessions remain visible with warning health.
- Run `cargo check -p threadlane-gpui` and `git diff --check`.

---

### Task 1: Reuse one loaded tree for startup session plan and history

**Files:**
- Modify: `src/state/app_state.rs:252-284, 654-675`
- Test: `src/state/app_state.rs` existing startup hydration tests

**Interfaces:**
- Add `load_session_projection(session_file: &Path) -> (SessionPlan, Vec<ChatMessageInfo>, usize, bool)`.
- Keep public `load_session_plan` and `load_session_messages` behavior unchanged by delegating through the existing loader where needed.

- [ ] **Step 1: Add a test proving startup derives plan and history from the same successful load path.**

Use the existing `app_state_startup_hydrates_complete_initial_session_history` test fixture and assert both the active plan and messages remain populated after construction.

- [ ] **Step 2: Implement a single-tree projection helper.**

Load `SessionTree` once, select the active branch with the existing fallback, project the page, and return `tree.plan().clone()` with the projected page metadata. On load failure return `(SessionPlan::default(), Vec::new(), 0, false)`.

- [ ] **Step 3: Update startup construction to call the helper once.**

Replace the separate `load_session_plan` and `load_session_message_page` calls in `AppState::new` with the tuple returned by `load_session_projection`.

- [ ] **Step 4: Run focused tests.**

Run `cargo test -p threadlane-gpui app_state_startup_hydrates_complete_initial_session_history` and expect PASS.

### Task 2: Avoid projecting the full history for newest-page loads

**Files:**
- Modify: `src/state/app_state.rs:262-284`
- Test: `src/state/app_state.rs` existing pagination tests

**Interfaces:**
- Keep `load_session_message_page(session_file, end)` return type unchanged.
- Add an internal `project_agent_message_page(messages: &[AgentMessage], end: usize)` helper that preserves the existing 40-message page and cursor behavior.

- [ ] **Step 1: Add/extend pagination assertions.**

Verify newest-page loading returns at most `CHAT_HISTORY_PAGE_SIZE`, reports `has_older` when appropriate, and preserves the newest message content.

- [ ] **Step 2: Implement page-aware projection without changing message ordering.**

First determine the required source range conservatively, project only that range when safe, and retain the existing full projection fallback for histories whose projected-message grouping cannot be bounded without scanning. Do not change tool-activity grouping or cursor semantics.

- [ ] **Step 3: Run pagination tests.**

Run `cargo test -p threadlane-gpui session_message_page_returns_newest_window_and_older_cursor` and expect PASS.

### Task 3: Cache unchanged session discovery metadata

**Files:**
- Modify: `src/state/app_state.rs:107-142, 194-250, 680-714`
- Test: `src/state/app_state.rs` new discovery cache tests

**Interfaces:**
- Add private `SessionDiscoveryCache` keyed by `PathBuf`, storing file size, modification time, and the resulting `SessionInfo`.
- Add `discover_sessions_in_project_cached(work_dir: &Path, cache: &mut SessionDiscoveryCache) -> Vec<SessionInfo>`.

- [ ] **Step 1: Add a test for unchanged-file reuse and changed-file invalidation.**

Create a temporary project session file, discover twice, assert the same metadata is reused, modify the file, discover again, and assert the entry is reloaded. Also assert deleted files disappear from returned sessions.

- [ ] **Step 2: Implement metadata-keyed cache invalidation.**

Use standard filesystem metadata (`len`, `modified`) as the cache key. Reuse cached `SessionInfo` only when both match; remove cache entries for paths no longer present; keep unreadable-file warning behavior.

- [ ] **Step 3: Route initial project discovery through the cache.**

Construct the cache before iterating registry projects and pass it into the cached discovery function. Preserve sorting and project behavior.

- [ ] **Step 4: Run discovery tests and check formatting.**

Run the focused discovery tests, then `cargo fmt --check`.

### Task 4: Reduce idle stream-loop work

**Files:**
- Modify: `src/screens/chat/view.rs:121-173`
- Test: existing state stream-drain tests if a pure helper is extracted

**Interfaces:**
- Preserve the existing GPUI stream task and scroll behavior.
- Use a slower idle timer than the current 33 ms while retaining 16 ms settling frames.

- [ ] **Step 1: Confirm the stream task has no behavior-specific timing dependency.**

Review existing stream and scroll tests; do not add timing-based tests.

- [ ] **Step 2: Change idle pacing and avoid redundant redraw requests.**

Retain immediate processing after each timer tick, use 16 ms only while settling, use a longer idle interval when no settling is needed, and call `cx.notify()` only when stream state changed or a scroll target was actually applied.

- [ ] **Step 3: Run the GPUI check.**

Run `cargo check -p threadlane-gpui`.

### Task 5: Final verification and review

**Files:**
- Review: all modified files and `docs/superpowers/plans/2026-03-10-session-history-performance.md`

- [ ] **Step 1: Run focused tests.**

Run `cargo test -p threadlane-gpui app_state`.

- [ ] **Step 2: Run required validation.**

Run `cargo check -p threadlane-gpui` and `git diff --check`.

- [ ] **Step 3: Review the diff.**

Confirm no generated files, unrelated formatting, duplicate persistence writes, or changed user-visible semantics are present.
