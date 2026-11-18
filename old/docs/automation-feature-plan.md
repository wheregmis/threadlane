# Threadlane automations plan

Status: implemented on `codex/sidebar-automations`. The domain store/scheduler lives in `threadlane-automation`, execution preparation in `threadlane-coding-agent`, the application service in `threadlane-ui-state`, and the management screen in `threadlane-ui-automation`.
Reference inspected: Synara commit `a33435c18474eb7816582004e45f87382965ac8d` on 2026-09-26. Source inspection only; neither application's automation UI was run for this study.

Implementation validation: focused domain, UI-state, sidebar, and GPUI editor tests cover persisted claims, DST, coalescing, revision conflicts, restart recovery, failure pausing, write failure, navigation isolation, and a real controller turn that waits for an explicit structured answer. The macOS development app was also opened to verify the sidebar destination, creation sheet, schedule selection, next-run preview, invalid-save feedback, and Escape dismissal. Git discovery in the editor runs off the UI thread so an OS access prompt cannot freeze the window. Live paid-provider scheduled runs, a full overnight sleep/wake cycle, and exhaustive theme/window-size coverage were not exercised.

Final checks: all 100 tests passed across `threadlane-automation`, `threadlane-ui-state`, `threadlane-ui-sidebar`, and `threadlane-ui-automation`; `cargo check -p threadlane-gpui` and `git diff --check` passed. A development app build also succeeded.

## Product decision

Add **Automations** as a stable sidebar destination beside Issues and Pull requests. An automation saves a prompt, project, model, execution environment, and schedule. Each initial-version run creates a normal durable Threadlane chat, so users review its transcript, changes, and permissions through existing surfaces.

The first release runs while Threadlane is open and the computer is awake. State this beside the schedule and in the empty state. Closing the window must not silently imply a background daemon exists. On restart or wake, coalesce missed occurrences into at most one run per automation.

## What to learn from Synara

| Observed implementation | Threadlane adaptation |
| --- | --- |
| Sidebar reads the same automation list and event stream as the automation page; its badge counts runs needing attention. | One service-owned projection; derive both list and badge from it. |
| Dedicated list/detail routes expose definitions, run history, editing, and pause/resume. | A native GPUI feature screen with list, details, and run history. |
| Modes are standalone (fresh thread), heartbeat (existing thread), and dedicated (automation-owned persistent thread). | Ship fresh chats first; add existing-chat continuation after lifecycle correctness is established. |
| Typed schedules include manual, once, interval, daily, weekdays, weekly, and cron. | Start with manual, interval, daily, weekdays, and weekly; defer raw cron and one-shot reminders. |
| Scheduler reconciles active runs before starting due work; sleeps until the next deadline with a periodic backstop and wakes on definition changes. | One application-owned Tokio service with the same ordering and event-driven wakeup. |
| Persistent scheduler leases, run records, and startup recovery separate scheduling from execution. | Persist occurrence identity before dispatch; recover against the durable session harness. |
| Results have attention/read/archive state; execution has separate statuses and permission snapshots. | Separate definition state, run lifecycle, and review state; link to the existing transcript instead of copying it. |

Source anchors: [sidebar](https://github.com/Emanuele-web04/synara/blob/a33435c18474eb7816582004e45f87382965ac8d/apps/web/src/components/Sidebar.tsx), [contracts](https://github.com/Emanuele-web04/synara/blob/a33435c18474eb7816582004e45f87382965ac8d/packages/contracts/src/automation.ts), [scheduler](https://github.com/Emanuele-web04/synara/blob/a33435c18474eb7816582004e45f87382965ac8d/apps/server/src/automation/Layers/AutomationScheduler.ts), [service and recovery](https://github.com/Emanuele-web04/synara/blob/a33435c18474eb7816582004e45f87382965ac8d/apps/server/src/automation/Layers/AutomationService.ts), [run reactor](https://github.com/Emanuele-web04/synara/blob/a33435c18474eb7816582004e45f87382965ac8d/apps/server/src/automation/Layers/AutomationRunReactor.ts), [schedule arithmetic](https://github.com/Emanuele-web04/synara/blob/a33435c18474eb7816582004e45f87382965ac8d/apps/server/src/automation/schedule.ts).

## User workflow

1. Open Automations without switching the active chat, composer project, or sidebar project filter.
2. See a compact list: name, project, schedule, next run, and latest outcome. Use an explicit All projects/project scope that stays independent of the active chat.
3. Select **New automation…**. A sheet collects name, prompt, attached project, existing model/effort selection, schedule, and checkout choice. Show the next three occurrences and timezone before saving.
4. Default Git projects to a fresh managed worktree per run. Allow the project checkout as an explicit choice; explain that runs can modify it. Non-Git projects use their directory. Never silently fall back from a failed worktree setup to the main checkout.
5. Save enables future scheduled runs; it does not run immediately. **Run now** explicitly creates one run through the same service path, including for a paused automation, without resuming its schedule.
6. Details expose **Run now**, **Pause/Resume**, **Edit…**, and an overflow menu containing Delete. Pause stops future dispatch; **Cancel run** separately stops current execution.
7. History shows time, status, and **Open chat**. Waiting permissions/questions open the existing chat controls. Completion never steals focus from the current chat.
8. Deleting a definition stops future scheduling and preserves run history, chats, and worktrees. Disable deletion while a run is active and offer Cancel run first. Marking a result reviewed only changes its review state.

Sidebar badge: count unresolved permission/question requests, failed/interrupted runs, and unreviewed completed runs once per run. Opening a completed result marks it reviewed; simply visiting the list does not clear actionable failures. Keep notification preference separate: default to needs-attention notifications, with an option for every completion.

Use existing GPUI Kit controls, shared theme tokens, and header clearance. Preserve keyboard activation, logical tab order, visible focus, accessible labels, and Escape/focus restoration for the sheet. Empty, loading, save-error, missing-project, unavailable-model, paused, and running states must all have explicit copy. Keep the primary actions visible rather than hover-only.

## Existing Threadlane seams

| Existing code | Reuse / required extension |
| --- | --- |
| `threadlane-ui-state/src/types.rs`: `WorkspacePage` | Add `Automations`; preserve separate active-session and navigation state. |
| `threadlane-ui-state/src/actions.rs` and `controller.rs` | Add the navigation action using the existing GitHub destination pattern. |
| `threadlane-ui-sidebar/src/view.rs` | Add the destination and attention count; include its projection revision in the sidebar fingerprint. |
| `threadlane-ui-workspace/src/view.rs` | Compose the feature view and bridge service events; keep scheduling logic outside the shell. |
| `threadlane-coding-agent/src/controller.rs`: `SessionController`, `spawn_interactive_turn`, cancellation and permission/question handles | Reuse execution ownership, event delivery, cancellation, and lifecycle. Add only the missing automation adapter. |
| `threadlane-coding-agent/src/scheduler.rs`: `AgentWorkScheduler` | Already schedules durable queue work inside a session. It is not a calendar scheduler; do not replace it or use its in-memory wakes as schedule persistence. |
| `threadlane-ui-state/src/chat.rs`: `execute_prompt` | Existing example of mapping controller events into session-scoped UI events. Avoid dispatch paths that assume the selected chat is the target. |
| `threadlane-project` registry and directory resolution | Resolve projects and global storage through these APIs. |
| Existing model catalog, credentials, harness, session discovery, and worktree operations | Reuse their canonical paths; no separate provider client, transcript store, project registry, or worktree implementation. |

Proposed ownership: one GPUI-free `threadlane-automation` crate for definitions, persistence, schedule calculation, and run-state transitions; one `threadlane-ui-automation` crate for the feature view. Execution adaptation belongs in `threadlane-coding-agent`; application-wide service lifecycle/projection wiring belongs in `threadlane-ui-state`. Dependencies point toward the automation domain, never from it into GPUI or the coding engine. Do not add a generic job framework or speculative plugin interfaces.

## Persistence and lifecycle contract

- **Definition:** stable ID, revision, name, prompt, canonical project identity, model ID and effort, checkout policy, typed schedule, enabled state, next occurrence, notification preference, timestamps. Credentials are resolved at execution time, never saved in the definition.
- **Run:** stable ID, automation ID, definition revision and execution snapshot, trigger, scheduled occurrence, session/harness identity, checkout identity, timestamps, status, error/summary, reviewed timestamp. Keep transcript content in the existing session JSONL.
- **States:** queued → starting → running → succeeded/failed/cancelled/interrupted; running may enter waiting-for-permission or waiting-for-answer. Skipped occurrences have an explicit reason. Waiting is not success and blocks another run of that automation.
- **Storage proposal:** versioned snapshot under `threadlane_project::global_threadlane_dir()/automations/`, using a serialized writer and atomic durable replacement. Keep occurrence claims, next-run advancement, and run creation in one commit. Reuse an existing atomic-write primitive if available; validate it before implementation. Use an OS-backed file lock for cross-process ownership; avoid stale PID-file locking.
- **Dispatch:** commit the run claim and assigned session ID before external execution. Attach automation/run identity to durable session metadata. Reconcile that identity after crashes; never blindly replay a possibly executed prompt. Exactly-once external side effects are not promised.
- **Recovery:** reconnect only to demonstrably live owned execution; derive terminal status from durable harness facts. If execution may have begun but cannot safely resume, mark interrupted and require an explicit new run. An automation-level failure must not stop the scheduler for other definitions.
- **Edits:** affect future runs only. An active run retains its revision snapshot. Resume schedules from the current time and does not replay paused history.
- **Retention:** no automatic transcript or worktree deletion. Page run history; define explicit history cleanup later if storage growth warrants it.

## Scheduling and execution rules

- One service per application, independent of which workspace page or window is visible; one cross-process scheduler owner. Use the shared Tokio runtime. Construct agents through `spawn_session_runtime_construction`, never GPUI's small-stack executor.
- Persist UTC instants and an explicit IANA timezone for wall-clock schedules. Machine timezone changes do not reinterpret saved definitions. During DST, skip a nonexistent local time and execute a repeated local time once. Show the exact preview produced by the scheduler.
- Interval schedules use a persisted anchor; daily/weekly schedules use local calendar arithmetic. Reuse a maintained time/timezone dependency after checking the dependency tree; do not implement timezone rules by hand. `chrono` is present transitively, but named-timezone support still needs verification.
- On wake/restart, coalesce missed occurrences to one latest due run, then move to the first future occurrence. Never enqueue a backlog for every missed interval. Use occurrence identity to survive clock rollback without duplicate dispatch.
- Default to one automation run at a time globally in v1, including waiting runs; clearly show queued work and which run blocks it. This limits background cost and simplifies ownership. No overlap for a definition; manual and scheduled triggers use the same guard.
- A missing project, unavailable credentials/model, failed worktree setup, or unresolved previous run is visible and actionable. Never change provider/model or checkout silently. Do not automatically retry side-effecting failed runs.
- Reuse configured permission policy without widening it. Existing controller construction enables interactive permission/question handling: retain visible requests while the application is open, mark the run as needing attention, and never auto-answer or dismiss them. Any future headless mode needs a separate explicit interaction policy.
- Proposed initial bounds: minimum interval one minute; one-hour active-execution timeout excluding time waiting for the user; pause the definition after three consecutive failed/interrupted executions and show the reason. Cancellation and timeout preserve partial work.
- Start with normal agent mode using existing provider routing. Enable ACP models only after their permission, cancellation, and completion events pass the same integration checks; show unsupported choices as unavailable rather than pretending parity.

## Implementation sequence and exit criteria

1. **Domain and durable store.** Add schemas, validation, atomic claims, revision snapshots, next-occurrence calculation, and injected-clock tests. Exit: duplicate claims, DST, sleep, restart, edits, pause, and malformed storage behave deterministically without GPUI.
2. **End-to-end Run now.** Create a durable background chat through existing session/worktree paths, drive the controller, and project progress. Exit: a run survives navigation, opens its ordinary transcript, respects permissions/questions, cancels correctly, and never changes the active chat implicitly.
3. **Scheduled lifecycle.** Add the app-owned timer/wakeup service, process ownership, recovery, coalescing, limits, and failure pausing. Exit: repeated scheduler passes and a crash at each claim/dispatch boundary do not duplicate execution; uncertain runs become interrupted.
4. **Sidebar and feature screen.** Add navigation, project scope, creation/edit sheet, list/detail/history, attention count, and review state. Exit: the complete create → scheduled run → review → pause flow works through pointer and keyboard in a real window.
5. **Release hardening.** Verify multiple windows/processes, missing worktrees, provider/auth failures, corrupted or unwritable storage, long prompts/names, light/dark themes, zoom, and minimum window size. Document app-open behavior and partial-work recovery.

Validation: focused `cargo nextest run -p <touched-package> <filter>` tests first; `cargo check -p threadlane-gpui` and `git diff --check` for Rust/UI changes. Run broader workspace tests when engine integration warrants them. Mount interaction tests beneath initialized GPUI Kit `Root`; visually verify the application before claiming UI completion.

## Follow-up scope

Chat creation is implemented through `create_automation`, forwarding to the application service with a session-scoped retry key. It defaults to the chat's owning project and saved model/effort, persists before reporting success, and returns the saved definition and next occurrence. The user must explicitly request automation; unclear scheduling details are clarified in chat.

Chat-creation validation: 11 focused domain, tool, and automation-service tests passed, including default resolution, provider-prefix preservation, save acknowledgment, duplicate retries after reload, conflicts, invalid timezone/project, and a stopped service channel. `cargo check -p threadlane-gpui` passed. No live provider was charged for this verification.

After v1 is reliable: existing-chat heartbeat mode (defer while busy, never steer an unrelated turn), automation-owned persistent chats, one-shot schedules, and cron.

Defer daemon/OS wake scheduling, remote execution, workflow graphs, AI-evaluated stop conditions, arbitrary retry policies, and automatic worktree cleanup. Each adds a separate lifecycle contract; none is needed for the first useful recurring-task feature.
