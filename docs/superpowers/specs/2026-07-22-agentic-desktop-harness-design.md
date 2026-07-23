# Agentic Desktop Harness Design

**Date:** 2026-07-22  
**Status:** Approved design

## Summary

`threadlane` will become a Makepad desktop harness that can run independent coding-agent tasks across multiple projects, keep them working while the user switches views, and manage skills and extensions without requiring manual JSON edits.

The implementation will extend the existing Rust harness. It will not embed Pi or replace `threadlane-agent` with another runtime. Pi supplies the resource-management reference model; the Makepad host-launcher proof of concept in `/private/tmp/host_launcher_poc_reference` supplies the persisted registry and in-app manager pattern.

## Current State

The repository already contains:

- a generic `Agent` loop with streaming events, tools, hooks, queuing, and compaction;
- a project-aware `CodingAgent` with context discovery, sessions, plan mode, and WASM extensions;
- JSONL session trees with branching;
- project-local extension discovery and persisted extension state;
- a Makepad GUI with chat, plan, activity, model selection, and a project/session sidebar.

The GUI currently owns one `CodingAgent`. It can list sessions from extra projects, but `activate_session` explicitly rejects a session whose project differs from the process working directory. Chat and activity are also stored in process-wide static collections, so they cannot represent concurrent tasks safely. Skill discovery and a capability-management UI do not exist.

## Goals

1. Run multiple coding-agent tasks concurrently, including tasks from different projects.
2. Keep each task bound to its own canonical workspace, session, events, status, and resources.
3. Make the primary workflow simple: add a project, create a task, enter a prompt, and switch away without stopping it.
4. Discover and progressively load Agent Skills from global and project scopes.
5. Provide robust extension discovery, validation, enablement, state, trust, and errors.
6. Manage skills, extensions, and packages from one GUI surface.
7. Preserve the current `Agent`, `CodingAgent`, session-tree, and WASM-extension implementations where they already fit.
8. Enforce workspace containment and explicit trust decisions at capability boundaries.

## Non-goals

- Embedding Pi or promising source-level compatibility with Pi TypeScript extensions.
- Building an online package registry before one exists.
- Adding prompt-template or theme marketplaces.
- Adding speculative distributed execution or daemon infrastructure.
- Supporting multiple simultaneous turns inside one task. Tasks run concurrently; each task serializes its own turns.

## Architecture

```text
Makepad App
  ├── Project/task navigation
  ├── Active conversation
  ├── Activity/plan/change inspector
  └── Capability manager
          │
          ▼
HarnessSupervisor
  ├── TaskRuntime(project A, session 1, CodingAgent, status, events)
  ├── TaskRuntime(project A, session 2, CodingAgent, status, events)
  └── TaskRuntime(project B, session 1, CodingAgent, status, events)
          │
          ▼
Project resources
  ├── context files
  ├── resolved skills
  ├── resolved extensions
  └── project settings
```

### Existing layers

- `Agent` remains the provider/tool loop. It knows nothing about projects or GUI state.
- `CodingAgent` remains the coding harness. It owns one workspace, session tree, context, policies, and resolved capabilities.
- `WasiExtensionManager` remains the sandboxed extension runtime and evolves behind its existing API.

### New supervisor

`HarnessSupervisor` owns a map of task IDs to `TaskRuntime` values. A task runtime contains:

- stable task and project IDs;
- canonical project root;
- one `CodingAgent` and session file;
- task status: idle, running, waiting, completed, failed, or cancelled;
- a per-task prompt lock;
- an event sender tagged with the task ID;
- the capability generation used for its next turn.

The supervisor exposes only the operations the GUI needs: register a project, create or restore a task, submit input, cancel a task, select a task, and refresh capabilities. Switching the selected task changes presentation state only; it does not stop background work.

No separate `ProjectRuntime` abstraction is needed initially. Project metadata and resolved resources are ordinary values cached by the supervisor and shared when tasks are created or refreshed.

## State and Event Flow

1. The app registers a project root and canonicalizes it.
2. Project context, settings, skills, and extensions are discovered.
3. Creating or restoring a task constructs a `CodingAgent` bound to that project and session.
4. Submitting a prompt acquires only that task's prompt lock.
5. Agent events are wrapped with the task ID and sent to the UI thread.
6. The GUI updates the matching task transcript, activity, status, and plan, whether or not it is selected.
7. Selecting a task reads its existing view state; no agent reconstruction is required.
8. Capability changes increment the project's capability generation. Idle tasks refresh immediately; running tasks refresh before their next turn.

The current process-wide chat, activity, and plan stores become task-keyed stores. Existing Makepad list widgets continue reading shared state, but select the active task's slice during drawing.

## Persistence

Project-owned data remains under the project:

```text
<project>/.threadlane/
  settings.json
  sessions/*.jsonl
  skills/<skill-id>/SKILL.md
  extensions/<extension-id>/extension.wasm
  packages/<package-id>/
  state/extensions/<extension-id>.json
```

Global data lives under `~/.threadlane/` with the same `skills`, `extensions`, and `packages` directories plus application settings. The project registry stores canonical paths, display names, last-selected task IDs, and recent ordering. All settings and extension-state updates use write-to-temporary-file followed by rename.

At startup, threadlane restores registered projects and saved tasks. A task that was running when the app stopped is restored as interrupted, not completed or still running.

## Skills

Skills use the Agent Skills `SKILL.md` format.

Discovery locations, in increasing precedence, are:

1. `~/.agents/skills/`
2. `~/.threadlane/skills/`
3. enabled global packages
4. `.agents/skills/` from project root
5. `<project>/.threadlane/skills/`
6. enabled project packages

A stable skill ID is its declared name. A later scope overrides an earlier skill with the same ID. Discovery reads only metadata needed for the catalog and system prompt. Full instructions load when the model selects the skill or the user invokes `/skill:<name>`.

Validation reports missing names, malformed front matter, unreadable instructions, and duplicate IDs. Invalid skills remain visible but disabled. Skills are instructions and may reference executable scripts, so the manager displays their source and trust scope.

## Extensions

### Sandboxed extensions

WASM remains the default extension format. The versioned host API supports:

- tools;
- slash commands;
- lifecycle hooks;
- host-persisted state;
- declared effects validated by the host.

The manager exposes manifest metadata, API version, source, scope, enabled state, declared capabilities, load errors, and current state location. An invalid extension cannot prevent other extensions or tasks from loading.

### Full-trust extensions

Full-trust extensions are optional executables declared by an installed package. They run as child processes and communicate through newline-delimited JSON. This avoids a Rust dynamic-library ABI and contains crashes, but it does not make the extension safe: the process retains host-user authority.

Before first launch, the GUI shows the executable path, package source, requested project/global scope, and an explicit trust action. Trust is stored against the package identity and source revision. A changed revision requires approval again. Denial leaves the extension installed but disabled.

## Packages

A package is a directory containing `threadlane-package.json` plus conventional `skills/` and `extensions/` directories. The manifest contains only what management requires:

- stable ID, name, and version;
- source metadata;
- resource paths;
- optional full-trust executable declaration.

Initial sources are local directories and Git URLs. Install copies or clones into the selected global or project package directory, validates before enablement, and records the resolved revision. Update, remove, enable, and disable operate through the same catalog. Online search and npm compatibility are deferred until an actual registry or compatibility requirement exists.

## GUI

The main window uses three regions:

```text
┌ Projects and tasks ─┬ Active conversation ───────────────┬ Inspector ──────┐
│ + Add project       │ Project / branch / model / mode    │ Activity        │
│                     │                                     │ Plan            │
│ ● Project A         │ Messages and tool results           │ Changes         │
│   ● Running task    │                                     │                 │
│   ◌ Waiting task    │                                     │                 │
│ ● Project B         │ Prompt…                       Send  │                 │
└─────────────────────┴─────────────────────────────────────┴─────────────────┘
```

- Adding a folder registers a project and creates its first task.
- Project rows contain task rows with visible running, waiting, completed, failed, and cancelled states.
- The conversation is the visual priority.
- The inspector keeps tool activity, plan progress, and changed files out of the transcript.
- A project-header action opens the capability manager.
- A command palette provides keyboard access without hiding essential actions behind commands.
- Focus states, keyboard traversal, non-color status indicators, readable contrast, and responsive panel collapse are required.

### Capability manager

One manager uses tabs or filters for Skills, Extensions, and Packages. Every row shows name, version when applicable, global/project scope, source, enabled state, validation state, and trust level. The available actions are install, update, enable, disable, remove, inspect, and trust/revoke trust when relevant.

The manager follows the temp host launcher's successful registry/store split: discovery and persisted state live outside the widget, while the widget renders entries and emits user actions.

## Security

Workspace isolation is enforced in the shared tool execution path, not in individual callers:

- project roots are canonicalized when registered;
- file paths are resolved and rejected when they escape the root;
- new-file paths resolve their nearest existing ancestor before creation, rejecting `..` and symlink escapes;
- command working directories must remain within the project root;
- symlink traversal cannot bypass containment;
- extension effects are validated by the host;
- full-trust execution requires explicit revision-bound approval.

The shell itself retains the user's host authority: restricting its starting directory is not a filesystem sandbox, and the GUI labels it as a full-trust tool. A portable command sandbox is outside this design; one can be added when threadlane has a concrete platform backend. File tools and caller-supplied command working directories are contained before concurrent cross-project execution because the current tools accept unrestricted paths and working directories.

## Error Handling

- Resource validation errors disable only the affected resource and remain visible in the manager.
- A task failure changes only that task's state and retains its transcript and diagnostics.
- Provider or authentication errors appear in the task and app status without discarding input.
- Cancelling a task stops future agent/tool work and marks interrupted activity.
- Corrupt session files are reported and left untouched; the user may create a new session.
- Persistence failures surface a notification and retain the in-memory state rather than pretending it was saved.

## Verification

The smallest checks that cover the new behavior are:

1. Unit tests for canonical workspace containment, resource precedence, skill metadata parsing, package validation, and tagged event routing.
2. One integration test that runs two task runtimes against different temporary projects and proves their tools, events, sessions, and extension state do not cross.
3. Valid and invalid skill/WASM fixtures.
4. A full-trust protocol fixture covering approval, revision change, malformed output, and process exit.
5. Makepad headless UI coverage for adding two projects, starting tasks, switching while one runs, visible statuses, and capability actions.
6. A fresh Studio run followed by screenshot and widget-tree inspection at desktop and narrow widths.

## Delivery Sequence

1. Enforce workspace containment in the shared tool layer.
2. Add task-tagged state and `HarnessSupervisor`; wire real cross-project task switching.
3. Add skill discovery, precedence, progressive loading, and `/skill:<name>`.
4. Extend extension metadata/validation and build the capability catalog.
5. Add package install/update/remove and optional full-trust child-process extensions.
6. Complete the capability manager and multi-project GUI states.
7. Run concurrency, persistence, headless UI, and Studio visual verification.

Each step must leave a runnable check. The implementation plan may split these steps into separate commits, but the accepted end state includes all seven.

## Acceptance Criteria

- Two tasks in different projects can run concurrently and keep isolated workspace roots, sessions, resources, events, and statuses.
- Switching the active GUI task does not stop or reconstruct another task.
- A user can add a project, create a task, prompt it, cancel it, and resume its saved session from the GUI.
- Global and project skills are discovered with documented precedence, validated, listed, and loaded only when used.
- Sandboxed extensions can be inspected, enabled, disabled, and diagnosed from the GUI.
- Local and Git packages can be installed, updated, disabled, and removed at global or project scope.
- Full-trust extension execution never occurs without revision-bound approval.
- File tools cannot escape the task's project root, and `run_command` rejects a caller-supplied working directory outside it while remaining visibly full-trust.
- Interrupted runs, resource errors, and persistence errors are represented honestly and locally.
- The complete workflow passes automated checks and fresh Makepad runtime inspection.

## References

- Pi packages: <https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/packages.md>
- Pi skills: <https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/skills.md>
- Pi extensions: <https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/extensions.md>
- Local GUI/registry reference: `/private/tmp/host_launcher_poc_reference`
