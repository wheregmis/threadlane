# Workspace Header Hierarchy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the flat workspace heading with a compact two-line project identity, intelligently compact long paths, and make the existing capabilities action visibly clickable.

**Architecture:** Keep presentation helpers and their tests in the existing app module because startup already owns the working directory and header labels. Update only the Makepad header DSL and startup label population; preserve the existing capabilities discovery and click event path.

**Tech Stack:** Rust, Makepad widgets/script DSL, Cargo tests

## Global Constraints

- Keep the existing capabilities click behavior unchanged.
- Add no dependencies or new navigation surfaces.
- Derive all header identity text from the active working directory.
- Display derivation must be non-failing.

---

### Task 1: Path and project identity formatting

**Files:**
- Modify: `crates/threadlane-gui/src/app/mod.rs`
- Test: `crates/threadlane-gui/src/app/mod.rs`

**Interfaces:**
- Consumes: `std::path::{Component, Path, PathBuf}` and optional home directory text from `std::env::var_os("HOME")`
- Produces: `project_name(path: &Path) -> String` and `compact_workspace_path(path: &Path, home: Option<&Path>) -> String`

- [ ] **Step 1: Write failing unit tests**

Add tests asserting that project names come from the final path component, `/` safely falls back to `/`, short home paths become `~/Documents/threadlane`, and long paths become `~/Documents/…/exploration/threadlane`.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p threadlane-gui workspace_header`
Expected: compilation failure because the formatting helpers do not exist.

- [ ] **Step 3: Implement minimal pure helpers**

Implement `project_name` using `file_name()` with a lossy-display fallback. Implement `compact_workspace_path` by stripping the provided home prefix, substituting `~`, retaining short paths, and replacing the middle of paths with more than four visible components using `…` while retaining the first component and final two components.

- [ ] **Step 4: Run focused tests**

Run: `cargo test -p threadlane-gui workspace_header`
Expected: all workspace header formatting tests pass.

---

### Task 2: Header presentation and wiring

**Files:**
- Modify: `crates/threadlane-gui/src/app/mod.rs:503-545`
- Modify: `crates/threadlane-gui/src/app/mod.rs:1014-1018`

**Interfaces:**
- Consumes: `project_name(path: &Path) -> String`, `compact_workspace_path(path: &Path, home: Option<&Path>) -> String`
- Produces: Makepad widgets with IDs `project_name_label`, `workspace_label`, and existing `caps_btn`

- [ ] **Step 1: Replace the flat left heading**

Create a folder icon followed by a two-line `View`: a bold `project_name_label` and a muted `workspace_label`. Keep the header height fit-based and compact.

- [ ] **Step 2: Strengthen the capabilities affordance**

Retain `caps_btn`, add a capabilities icon and chevron through the button's icon/text styling where supported by the existing Makepad button primitive, and strengthen transparent/default, hover, down, border, and text colors. Do not alter its action handler.

- [ ] **Step 3: Populate both identity labels at startup**

Set `project_name_label` from `project_name(&work_dir)` and `workspace_label` from `compact_workspace_path(&work_dir, std::env::var_os("HOME").as_deref().map(Path::new))` before session refresh.

- [ ] **Step 4: Format and run the GUI crate tests**

Run: `cargo fmt --all -- --check && cargo test -p threadlane-gui`
Expected: formatting check and all GUI tests pass.

- [ ] **Step 5: Compile the GUI crate**

Run: `cargo check -p threadlane-gui`
Expected: compilation succeeds with no errors.

- [ ] **Step 6: Review the focused diff**

Run: `git diff --check && git diff -- crates/threadlane-gui/src/app/mod.rs`
Expected: no whitespace errors; diff contains only path helpers/tests, header DSL, and startup wiring for this feature.
