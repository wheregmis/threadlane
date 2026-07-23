# Multi-Project Concurrent Workspaces Implementation Plan

> **Goal:** Let users attach project folders, create and switch sessions across them, and keep tasks running concurrently while another project is active.

**Architecture:** Extend the GUI's existing project-keyed state instead of replacing it. `SessionKey { work_dir, session_id }`, `AppState`, `session_runtimes`, and generation events already isolate active and background sessions by canonical project path. Add a durable global project registry, remove remaining `current_dir()` routing, and make every header/session/action derive from the active `SessionKey`. Keep `CodingAgent` bound to one canonical project root. `HarnessSupervisor` remains a future consolidation target; migrating the GUI to it is not required for this feature because its current runtime lacks the GUI's image, draft restoration, generation-correlation, and cancellation behavior.

**Tech Stack:** Rust, Makepad widgets, `robius-file-picker`/native folder dialog support, Tokio, JSON persistence, Cargo tests

## User Experience

1. The sessions sidebar has an **Add Project** action in its header.
2. Clicking it opens a native folder picker.
3. Selecting a folder registers the canonical path globally and shows a project group immediately.
4. A newly attached project gets an untitled draft; the user can create its first session with the project-row `+` action or by sending a prompt.
5. Clicking any session switches the visible transcript, draft, attachments, model, effort, project title, and path without stopping work elsewhere.
6. Running sessions in inactive projects continue receiving events and show a running indicator in the sidebar.
7. Project context, skills, agents, extensions, tools, and session files always resolve from that session's own project root.
8. A project can be detached from the sidebar without deleting its `.threadlane` data or stopping a running session. Detach is blocked while that project has running sessions.

## Global Constraints

- Canonical project paths are the identity boundary; duplicate aliases and symlink aliases must not create duplicate projects.
- Switching views must not reconstruct or stop an existing `CodingAgent` runtime.
- One session serializes its own turns, but different sessions may run concurrently.
- Session and extension data remain project-local under `<project>/.threadlane/`.
- The attached-project registry is application-global under `~/.threadlane/gui/projects.json` and is written atomically.
- Attaching or detaching a project never changes the process working directory.
- Do not weaken workspace containment in `threadlane-tools` or allow one runtime to execute against another project's root.
- Preserve drafts, attachments, model, effort, transcript, streaming state, and status independently per session.
- No session files or project files are deleted when a project is detached.

---

## Task 1: Introduce a durable canonical project registry

**Files:**
- Create: `crates/threadlane-gui/src/panels/sessions/project_registry.rs`
- Modify: `crates/threadlane-gui/src/panels/sessions/mod.rs`
- Modify: `crates/threadlane-gui/src/panels/sessions/state.rs`
- Test: `crates/threadlane-gui/src/panels/sessions/project_registry.rs`

**Interfaces:**

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachedProject {
    pub path: PathBuf,
    pub display_name: String,
    pub attached_at: u64,
    pub last_opened_at: u64,
}

pub struct ProjectRegistry { /* global registry path + records */ }

impl ProjectRegistry {
    pub fn load(global_threadlane_dir: &Path) -> Result<Self, ProjectRegistryError>;
    pub fn attach(&mut self, raw_path: &Path) -> Result<AttachedProject, ProjectRegistryError>;
    pub fn detach(&mut self, canonical_path: &Path) -> Result<bool, ProjectRegistryError>;
    pub fn touch(&mut self, canonical_path: &Path) -> Result<(), ProjectRegistryError>;
    pub fn projects(&self) -> &[AttachedProject];
}
```

- [ ] **Step 1: Write failing registry tests**

Cover canonicalization, duplicate symlink/path aliases, nonexistent paths, files instead of directories, stable display names, detach behavior, malformed JSON recovery/reporting, and atomic save through a temporary file.

- [ ] **Step 2: Run the focused tests and confirm failure**

Run: `cargo test -p threadlane-gui project_registry`

- [ ] **Step 3: Implement registry loading and atomic persistence**

Store records in `~/.threadlane/gui/projects.json`. Resolve the global directory through one helper so tests can inject a temporary directory. Preserve malformed registry files by returning an actionable error instead of silently overwriting them.

- [ ] **Step 4: Replace `.threadlane/gui/sidebar_projects.json` loading**

Remove `load_extra_project_dirs(work_dir)`. Change `refresh_sessions` to accept the registry's canonical project list rather than treating process CWD as the owner of all sidebar configuration.

Suggested boundary:

```rust
pub fn refresh_sessions(project_dirs: &[PathBuf]) -> Vec<SessionListRow>;
```

- [ ] **Step 5: Keep active selection stable across refreshes**

If the active `(work_dir, session_id)` still exists, preserve it. If only the project remains, preserve a project draft selection. Otherwise select the most recently used attached project without implicitly creating a session.

- [ ] **Step 6: Run focused state tests**

Run: `cargo test -p threadlane-gui project_registry && cargo test -p threadlane-gui sessions`

---

## Task 2: Add the project attachment UI

**Files:**
- Modify: `crates/threadlane-gui/src/app/mod.rs`
- Modify: `crates/threadlane-gui/src/state.rs`
- Modify: `crates/threadlane-gui/Cargo.toml` only if native folder selection is unavailable through the current picker
- Test: `crates/threadlane-gui/src/app/mod.rs` or a new pure action-state test module

**Interfaces:**

Add GUI events that carry folder-picker results back to the Makepad UI thread:

```rust
GuiAgentEvent::ProjectFolderPicked(Result<Option<PathBuf>, String>)
```

- [ ] **Step 1: Verify folder-picker capability**

`robius-file-picker::FileDialog` currently exposes file/media selection but not folder selection. Prefer upgrading or using its folder API if one is available in the pinned revision. Otherwise add a direct `rfd` dependency matching the already-transitive desktop backend and use `pick_folder` off the UI thread. Do not simulate folder selection by asking the user to choose an arbitrary file.

- [ ] **Step 2: Add an explicit sidebar header**

Add a compact `Projects` row above `session_list` with `add_project_btn`. Give it accessible text/tooltip semantics rather than relying on an unlabeled `+` icon.

- [ ] **Step 3: Wire folder selection**

On click, open the picker. On success, call `ProjectRegistry::attach`, refresh groups, create/select a project draft workspace, update the header, and focus the composer. Cancellation is a no-op; errors become system messages without losing the active workspace.

- [ ] **Step 4: Handle duplicate attachment gracefully**

Selecting an already attached canonical folder selects that project instead of creating another row.

- [ ] **Step 5: Add detach action**

Extend the project-row context/action surface with `Detach Project`. Block detach if `working_sessions` contains any key for that project. Detach only removes the global registry entry and UI runtimes that are idle; it never removes `<project>/.threadlane`.

- [ ] **Step 6: Test pure attach/detach action transitions**

Cover successful attach, duplicate attach, cancel, picker error, detach-current fallback, and detach blocked by a running session.

---

## Task 3: Make active project identity first-class

**Files:**
- Modify: `crates/threadlane-gui/src/workspace/mod.rs`
- Modify: `crates/threadlane-gui/src/app/mod.rs`
- Modify: `crates/threadlane-gui/src/panels/sessions/state.rs`
- Test: `crates/threadlane-gui/src/workspace/mod.rs`
- Test: `crates/threadlane-gui/src/app/mod.rs`

**Interfaces:**

Add a project draft key helper rather than using a process-wide literal draft:

```rust
impl SessionKey {
    pub fn project_draft(work_dir: PathBuf) -> Self;
    pub fn is_draft(&self) -> bool;
}
```

- [ ] **Step 1: Add project-draft isolation tests**

Prove two projects can each retain an independent unsaved draft, attachments, transcript placeholder, model, and effort state.

- [ ] **Step 2: Centralize active project access**

Add helpers such as `active_work_dir()` and `active_session_key()`. UI actions must use these helpers instead of `std::env::current_dir()`.

- [ ] **Step 3: Update workspace header on every selection**

Move project name/path population out of startup-only code. `select_workspace_ui` must update `project_name_label` and `workspace_label` from the selected key's `work_dir`.

- [ ] **Step 4: Preserve session-specific controls**

When switching sessions, save and restore draft, attachments, model, reasoning effort, status, and action visibility. Do not use globally selected model/effort as the source of truth when a runtime already exists.

- [ ] **Step 5: Refresh project-scoped capabilities on selection**

The visible skill/agent count and capability catalog must represent the active project. Cache discovery results by canonical project path; do not rebuild a running agent merely to refresh header metadata.

- [ ] **Step 6: Run workspace tests**

Run: `cargo test -p threadlane-gui workspace`

---

## Task 4: Enable cross-project session activation

**Files:**
- Modify: `crates/threadlane-gui/src/app/mod.rs`
- Modify: `crates/threadlane-gui/src/panels/sessions/state.rs`
- Test: `crates/threadlane-gui/src/app/mod.rs`
- Test: `crates/threadlane-gui/src/workspace/mod.rs`

- [ ] **Step 1: Write a failing cross-project activation test**

Create two temporary projects with one session each. Activate project A, then project B, and assert the active key, header, transcript, runtime work directory, and selected session all change to B without removing A's runtime.

- [ ] **Step 2: Remove the explicit cross-project rejection**

Delete the `entry.work_dir != current_dir()` early return in `activate_session`. Canonicalize the entry path and verify it remains attached before constructing a runtime.

- [ ] **Step 3: Construct agents from the selected session's project**

Continue passing `entry.work_dir` into `CodingAgentOptions`. Load project B's session file and restore its transcript into the B-keyed workspace only.

- [ ] **Step 4: Update last-used registry state**

Touch the selected project's `last_opened_at` and remember its last selected session ID. Persist this so startup can restore the same project/session.

- [ ] **Step 5: Verify inactive runtime preservation**

Assert switching does not remove, replace, relock, or cancel the runtime stored under project A's `SessionKey`.

---

## Task 5: Remove process-CWD routing from session creation and commands

**Files:**
- Modify: `crates/threadlane-gui/src/app/mod.rs`
- Modify: `crates/threadlane-gui/src/panels/sessions/state.rs`
- Test: `crates/threadlane-gui/src/app/mod.rs`

- [ ] **Step 1: Inventory every remaining `std::env::current_dir()` call**

Classify each use as startup default only or active-project routing. Startup may attach/select the launch directory once; all later user actions must derive from `SessionKey.work_dir` or a selected `ProjectGroup`.

- [ ] **Step 2: Fix first-send session creation**

In `dispatch_input`, create a session under `active_key.work_dir/.threadlane/sessions`, not process CWD. If the active key is a project draft, replace it with the newly created session key while preserving draft and attachments.

- [ ] **Step 3: Fix refresh and fallback paths**

Archive, delete, generation completion, and session creation must refresh all registered projects or the affected project—not rebuild from process CWD. Fallback selection must prefer another session in the same project, then that project's draft, then another attached project.

- [ ] **Step 4: Keep slash commands project-local**

Commands execute against the active runtime. `/compact`, `/model`, `/session`, skills, prompt templates, and extension commands must never resolve through the startup directory.

- [ ] **Step 5: Add routing regressions**

With process CWD set to project A and active workspace set to project B, verify first send, session creation, session refresh, and agent tools all use project B.

---

## Task 6: Prove concurrent cross-project operation

**Files:**
- Modify: `crates/threadlane-gui/src/app/mod.rs` as needed
- Modify: `crates/threadlane-gui/src/workspace/mod.rs` as needed
- Test: `crates/threadlane-gui/src/workspace/mod.rs`
- Test: `crates/threadlane-gui/tests/multi_project_runtime.rs` (new integration test if GUI internals can be exposed cleanly)

- [ ] **Step 1: Add a two-project runtime harness**

Use temporary project roots and deterministic/mock agent events. Avoid real provider calls. Create one session runtime per project and assign independent generation IDs.

- [ ] **Step 2: Start A, switch to B, then start B**

Verify A remains running after B becomes active, B can start independently, and both entries appear in `working_sessions` with canonical `(work_dir, session_id)` keys.

- [ ] **Step 3: Route interleaved events**

Send text deltas, tool events, completion, and errors for A and B in interleaved order. Verify each transcript/status changes only under its matching `SessionKey`, including when inactive.

- [ ] **Step 4: Verify cancellation isolation**

Stopping A must abort only A's generation handle, restore only A's submitted draft/attachments, and leave B running.

- [ ] **Step 5: Verify project resource isolation**

Give A and B distinct `AGENTS.md`, skills, extension state, and files. Assert each `CodingAgent` discovers only its own project resources and writes session data only beneath its own root.

- [ ] **Step 6: Verify tool containment**

Exercise file and command tools with paths attempting to reach the other temporary project. Expected: rejection by the shared workspace containment layer. If containment is incomplete, stop and fix it before shipping multi-project concurrency.

---

## Task 7: Restore attached projects and sessions at startup

**Files:**
- Modify: `crates/threadlane-gui/src/app/mod.rs`
- Modify: `crates/threadlane-gui/src/panels/sessions/project_registry.rs`
- Modify: `crates/threadlane-gui/src/panels/sessions/state.rs`
- Test: corresponding modules

- [ ] **Step 1: Load the global registry before session discovery**

If the launch CWD is not registered, attach it as the initial project once. Then discover sessions for all attached projects.

- [ ] **Step 2: Restore selection without eagerly creating every runtime**

Restore only the active project's last selected session runtime at startup. Construct other runtimes lazily when selected. Saved session rows remain visible immediately.

- [ ] **Step 3: Handle missing/moved projects**

Keep a disabled registry record with a visible `Missing` state instead of silently deleting it. Offer `Locate…` and `Detach`; do not scan unrelated directories automatically.

- [ ] **Step 4: Mark interrupted work accurately**

No background task survives process exit. Any persisted running marker is restored as interrupted/idle, never as actively running.

- [ ] **Step 5: Add startup restoration tests**

Cover two attached projects, last selected project/session, missing project, malformed session file, and no existing sessions.

---

## Task 8: Final UI and repository verification

- [ ] Run formatting:

```sh
cargo fmt --all -- --check
```

- [ ] Run focused GUI tests:

```sh
cargo test -p threadlane-gui
```

- [ ] Run coding-agent and tool isolation tests:

```sh
cargo test -p threadlane-coding-agent
cargo test -p threadlane-tools
```

- [ ] Run the full workspace tests:

```sh
cargo test
```

- [ ] Run diagnostics and compile the GUI:

```sh
cargo check -p threadlane-gui
```

- [ ] Perform manual desktop verification:

1. Attach two folders through the native picker.
2. Create one session in each project.
3. Start a long-running task in project A.
4. Switch to project B and start another task.
5. Switch repeatedly while both stream; verify no transcript/status crossover.
6. Change model/effort independently in each session.
7. Stop A and verify B continues.
8. Quit and reopen; verify projects and last selection restore.
9. Detach an idle project; verify its files and `.threadlane` directory remain untouched.
10. Reattach it; verify previous sessions reappear.

## Acceptance Criteria

- [ ] Users can attach project folders without editing JSON manually.
- [ ] Attached projects persist globally across app launches.
- [ ] Duplicate and symlink-equivalent paths produce one project.
- [ ] Sessions can be created and activated in any attached project.
- [ ] Two sessions from different projects can run concurrently.
- [ ] Switching projects does not stop or reconstruct inactive runtimes.
- [ ] Drafts, attachments, transcripts, model settings, status, and generation events remain isolated by `SessionKey`.
- [ ] Header and capability metadata always represent the active project.
- [ ] All post-startup routing uses the active project's canonical path rather than process CWD.
- [ ] Project-local sessions, resources, extensions, and tools never cross workspace boundaries.
- [ ] Detaching a project is non-destructive and blocked while that project is working.
- [ ] Startup restores attached projects and the last selected session without pretending interrupted tasks are still running.
