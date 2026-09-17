# Threadlane UI/UX plan — lessons from Synara

Prepared 2026-09-17. Implementation authorized as an active goal on 2026-09-17.

## Implementation progress

- [x] Phase 1 — Shell and hierarchy. Implemented compact composer, shared rem-based content width, Tools menu, branch context, task labels, quieter sidebar metadata, and compact footer. Sidebar bounds and keyboard test passes at 223 logical pixels. Native layouts inspected at 1024×768, 1280×800, 1440×900, and maximized size; the final normal launch size remains 1100×720.
- [x] Phase 2 — Conversation and results. Implemented clickable task-plan disclosure, completed-activity disclosure, keyboard-accessible tool rows, Jump to latest, loading-history presentation, and explicitly scoped workspace changes. History-following, long-plan bounds, plan isolation, and request-bound permission-detail regression tests pass.
- [x] Phase 3 — Context and review. Implemented direct Review action, rem-based panel constraints, temporary narrow-window navigation collapse, review focus mode, and composer focus restoration. In-panel diff preview, file navigation, and selection-preserving refresh were inspected. Divider preferences are separate from temporary window constraints; a 340px custom width survived hidden-panel resize, maximize, reopen, and return to the original window.
- [x] Phase 4 — Settings and polish. Reused labeled native kit buttons for settings navigation, shared rem-based reading width, and named shell controls. Existing light/dark text colors pass the measured contrast checks below. Light and dark Appearance cards and conversation/Review layouts were inspected; Dark was restored. Permission and question cards were inspected at 125% text size. Keyboard and overlay checks cover the changed controls through native journeys and real GPUI interaction tests.
- [x] Acceptance journeys 1–8 and final design-guide audit. Evidence and verification limits are recorded in the final acceptance table below.

Acceptance status: the normal app completed project discovery and the final native Git-dialog, history, and trajectory checks passed. Journeys 1–8 have representative native evidence, supplemented by exact GPUI state/focus/scroll regressions. A custom 340px Review split survived hidden-panel resize. All 169 UI/state tests passed; the final accessible-name-only editor change passed compilation and build. The development bundles contain the final build.

The full plan remains the completion target. Partial implementation or a successful build does not complete the goal.

## Direction

Make Threadlane feel like a calm workspace for directing an agent: find the task, understand its state, give input, inspect the result. Borrow Synara’s visual hierarchy and progressive disclosure while preserving Threadlane’s session, project, permission, and terminal contracts.

The first release should improve the existing shell, sidebar, transcript, composer, and review workflow. It does not need new navigation systems, a new UI framework, or feature parity with Synara.

## Evidence and limits

Synara was inspected with Computer Use through its native accessibility tree and screenshots: an active conversation, expanded tool output, plan summary above the composer, environment panel, split diff review, new-thread screen, provider menu, and General settings. No messages were sent and no settings were changed.

Threadlane’s installed app was inspected as an initial baseline. Its visible workspace has a flat task list, project filter, Needs you / Working / Ready status controls, metadata-rich task rows, Chat / Trajectory / Editor controls, project and execution-mode controls above the composer, and a global status bar. Development-build verification is recorded below.

Source inspection confirms existing implementation owners and reusable components. This is a targeted review, not a full source audit. Tokensave tools were unavailable in this session and `.tokensave/tokensave.db` was absent, so targeted file searches were used. No extractor limitation was established.

Observed Synara behavior and proposed Threadlane behavior are separated below. Dimensions are proposed starting constraints, not measured Synara specifications. Light mode, VoiceOver, scaling, minimum-width layouts, and error/permission flows require implementation-time validation.

## What to borrow

| Surface | Observed in Synara | Threadlane proposal |
| --- | --- | --- |
| Navigation | Quiet project rows, restrained selection, task state near the title | Keep the flat list and project filter; give titles more space and reduce routine badges |
| Conversation | Readable central column; compact tool rows with expandable details | Keep existing activity groups; emphasize the response and current actionable state |
| Composer | Input and a compact control strip; project/mode/branch nearby | One coherent input surface with context above and execution controls below |
| Progress | Plan summary directly above the composer | Reuse the existing session plan tracker, collapsed to current step and completion count |
| Environment | Branch, changes, repository, PR, usage, and editor entry points grouped together | Consolidate existing context into the right panel; show only data already available |
| Review | Diff opens beside the conversation and receives substantial width | Reuse Review and its selection state; open it directly from an explicit changes summary |
| Empty task | One prompt, visible project, bottom composer | Refine the existing new-task screen and preserve the same composer geometry |
| Settings | Persistent category navigation, section groups, descriptions next to controls | Restyle existing settings pages with consistent row alignment and readable section widths |

Do not copy Synara’s entire feature list, very faint secondary text, or densely packed icon toolbars. Kanban, automations, standalone chats, simulator panels, voice, and recap generation are separate product decisions.

## Proposed window composition

```text
┌──────────────────┬─────────────────────────────────┬──────────────────────┐
│ Native controls  │ Task title            Tools ▾   │ Review / Files / Web │
│ New task     ⌘N  ├─────────────────────────────────┤                      │
│ Search tasks     │                                 │ Project · Branch     │
│ All projects  ▾  │ Conversation                    │ Changes · PR         │
│                  │                                 │                      │
│ Needs you 2      │ Working · current operation      │ Selected surface     │
│ Working 1        │ ▸ Completed activity             │                      │
│                  │                                 │                      │
│ Recent tasks     │ Plan · 2 of 4 · current step     │                      │
│ Selected task    │ 5 files changed        Review    │                      │
│ Other task       │ Project · Local/worktree · Branch│                      │
│                  │ ┌─────────────────────────────┐ │                      │
│                  │ │ Ask for changes…            │ │                      │
│                  │ │ +  Model ▾  Effort ▾   Send │ │                      │
│ Settings…        │ └─────────────────────────────┘ │                      │
└──────────────────┴─────────────────────────────────┴──────────────────────┘
                    Optional project terminal below the work area
```

The right panel is dismissible; the conversation consumes the space when closed. Keep the terminal in its existing bottom region. A new permanent environment column beside a separate tool column would consume too much space.

### Size and visual rules

| Region | Minimum | Comfortable default | Surplus behavior |
| --- | --- | --- | --- |
| Sidebar | 13rem | 15rem | Resize up to 20rem; collapse through a visible control |
| Conversation | 28rem with navigation collapsed if needed | Prose around 48rem | Center prose and composer; permit wider code/table blocks |
| Right panel | 20rem | 26rem | Review may grow to roughly half the work area while keeping conversation usable |
| Terminal | About 8rem high | About 14rem | User-resizable; clamp restored height to leave usable conversation space |

Prototype at 1440×900, 1280×800, and 1024×768 logical window sizes. These are validation targets, not a new enforced window minimum. At narrow widths, close the auxiliary panel before squeezing the composer; provide a focus mode for review if both panes cannot meet their minima. Preserve the user's preferred split separately from temporary width constraints.

Use the existing semantic theme colors, spacing scale, component sizes, and themed radii. Keep the main canvas flat, with subtle sidebar/panel differentiation and one separator per boundary. Accent color should identify selection or a primary commitment; success color should communicate a meaningful successful state, not routine metadata. Keep body copy readable, and use monospace only for code and technical values. Match transcript and composer leading edges.

## Surface specifications

### 1. Sidebar: scan titles first

Keep New task, search, project scope, status filters, recent tasks, and Settings. Change the visible “New Session” label to “New task” consistently across tooltips and menus while retaining the internal session model.

Use two row levels: title and time first, then muted project plus one useful status. A PR indicator can remain when relevant, but detailed check counts, comment counts, and branch information belong in the task’s details or tooltip. Keep Needs you prominent, Working legible, and routine completed state neutral. Long titles truncate with a full-title tooltip; trailing state/actions occupy stable slots.

Retain the existing flat list. Synara’s project nesting is useful inspiration for grouping, but replacing Threadlane’s project filter would change an explicit product contract. Filtering must never change the active session or composer project. Archive/delete keep the existing action path; secondary actions remain available through an accessible menu and keyboard, even when visually quiet at rest.

### 2. Task header and context

Give the task title the leading position. Keep Chat primary; retain Editor and Trajectory as accessible tools, with secondary labels moving into a menu at narrow widths. Do not remove Trajectory functionality.

Keep project, execution location, and branch visible near the composer because they explain where the next instruction runs. Condense duplicated model/branch information in the global footer after equivalent context is available elsewhere. Put detailed token, turn, and tool counts in their existing context/trajectory surfaces. Unknown usage must say “Unavailable,” never imply zero consumption.

### 3. Composer: one stable control area

Reduce excess empty height at rest and allow multiline input to grow to a bounded height, then scroll internally. Align the input, attachments, permission/question cards, plan, and transcript to one content column.

The control strip contains attachment/extras, model, supported effort, context status, and Send or Stop. Keep meaningful provider identity, but shorten repeated provider/model prefixes visually; preserve the exact persisted model ID and full tooltip. Use the existing live catalog and ACP configuration paths. Do not add static model lists or expose native effort controls for ACP agents.

While running, make Queue and supported Steer behavior explicit using the existing execution paths. Stop remains unmistakable. Preserve the current input and draft when changing tasks or opening panels. Enter/Shift+Enter and scoped slash-menu Tab behavior must stay consistent with the app’s keybindings. Do not introduce permission approval shortcuts into ordinary typing.

### 4. Transcript, progress, and results

Keep user messages subtly contained and assistant output mostly on the canvas. Completed tool activity collapses into a short summary; the active operation shows a verb, object, elapsed state, and disclosure. Errors stay visible with a specific recovery action; collapse must never hide a pending permission or question.

Reuse the existing activity groups and plan tracker. The compact plan displays completion count and current step; expansion shows the active session’s durable plan only. Do not derive it from tool history or turn it into a second task manager.

Expose “N files changed” with a labeled Review action when reliable change data exists. Label its scope explicitly: workspace changes are not automatically changes from this turn. Do not promise per-turn diffs until provenance is supported. Opening Review selects the corresponding file when known and retains conversation scroll position.

Autoscroll only while the user is following the end. When reading older content, retain position and offer Jump to latest. Expanding tool output must not unexpectedly move the reader to a different message.

### 5. Right panel and terminal

Extend the existing context header rather than introducing a second environment state store. Group project/branch, change summary, and PR status above Review / Files / Browser content. Add usage or richer environment sections only if existing data supports a concrete need.

Review needs readable width, a visible diff scope, stable file selection across refresh, and a clear close control. Use unified diff when available width cannot support two readable columns. Preserve existing Git action safety and confirmation behavior; visual simplification does not change destructive-action semantics.

The terminal remains project-owned, with independent persistent shell tabs. Closing its panel hides it rather than killing shells. Switching sessions within one project retains output and active tab; switching projects selects that project's group.

### 6. Empty state and settings

The existing “What should we build in [project]?” screen already follows the reference’s structure. Improve its spacing and context placement rather than replacing it. With no project/provider, name the missing prerequisite and offer the existing attach/connect action. Do not seed fake recent tasks or invented example activity.

Settings keeps its existing categories. Use a readable content width, concise section titles, aligned label/description/control rows, and visible connection state. Keep a clear return-to-workspace action. Search can follow later if category navigation proves insufficient; do not build a settings search index in the first pass.

## State and accessibility contract

| State | Required presentation and recovery |
| --- | --- |
| Empty task/list | Explain scope and expose New task, Attach project, or Clear filter as applicable |
| Loading history | Keep the selected task identity; show loading instead of a false empty transcript |
| Running | Current operation and Stop visible; activity details optional |
| Needs permission | Explicit action, target, scope, and visible decision buttons; request-bound details |
| Needs answer | Question card remains pending until Send answers or Dismiss |
| Provider error/offline | Preserve draft/transcript; show failure and relevant Retry or connection action |
| Missing worktree | Task stays visible; “Not checked out” in sidebar/composer; canonical-project PR lookup survives |
| Git read-only/unavailable | Disable only affected actions with a reason; retain inspectable content |
| No changes | Clear “No changes” state; no fabricated result summary |

Every icon-only control needs a name and tooltip. Selection, hover, focus, and open menus must remain visually distinct. Keyboard order follows navigation → task content → composer → auxiliary panel; no critical action is hover-only. Escape dismisses only the topmost dismissible overlay and restores focus. Focused GPUI elements require an accessible role. Test contrast rather than reproducing Synara’s faintest text.

## Implementation sequence and reuse map

| Phase | Work and existing owners | Exit condition |
| --- | --- | --- |
| 1 — Shell and hierarchy | `threadlane-ui-theme/src/theme.rs`, bundled theme; `threadlane-ui-sidebar/src/view.rs`; `threadlane-ui-workspace/src/view.rs`; chat header/composer in `threadlane-ui-chat/src/view.rs` | One coherent visual scale; more readable task titles; compact composer; no lost controls at target widths |
| 2 — Conversation and results | Existing `render_activity_group`, `render_plan_tracker`, `render_composer`, transcript rendering, and context meter | Running, completed, error, permission, question, and long-history states remain understandable and keyboard-operable |
| 3 — Context and review | Existing `threadlane-ui-right-panel/src/view.rs`, `types.rs`, selection tests, workspace resizable state | Changes open the correct scope/file; close restores focus; selection and scroll survive refresh/project transitions |
| 4 — Settings and polish | Existing `threadlane-ui-settings/src/view.rs`, shared theme/assets | Settings hierarchy matches the shell; long labels, themes, scaling, focus, and missing-worktree states pass review |

Start with Phase 1 as one reviewable change. Validate it in the real app before widening scope. Reuse existing components and state owners; no new crate or dependency is needed for these visual changes. Shared tokens belong in `threadlane-ui-theme`; product code stays out of the binary shell. Verify the current GPUI API before implementation.

## Acceptance and verification

For each phase, run `cargo check -p threadlane-gpui`, `git diff --check`, and focused Nextest coverage for changed behavior. Broaden tests only for cross-feature changes. A successful build does not establish visual or interaction correctness.

Manually complete these journeys in the development app:

1. Start a task, identify project/mode/model, type a multiline prompt, use autocomplete, and retain the draft through navigation.
2. Filter tasks without switching the active task or changing composer scope; clear the filter and recover the same selection.
3. Read older messages while new activity arrives; expand a tool and return to latest without scroll jumps.
4. Open changed files in Review, refresh, switch files, and close the panel without losing conversation position.
5. Resolve a test permission and question through their explicit controls; verify stale details cannot resolve a newer request.
6. Switch sessions and projects with multiple shells open; verify the documented terminal ownership.
7. Restore a task whose worktree is missing and inspect its transcript and PR context.
8. Navigate every control by keyboard, open/dismiss overlays, test long labels, light/dark themes, larger text, and narrow windows.

Compare intended shared edges and gaps at resolved bounds, including display scaling; do not fix drift with isolated offsets. Verify screen-reader roles and text contrast. Ask a reviewer to identify the active task, its project, whether attention is required, and the next action without opening menus.

Design-guide review: task and scope are explicit; hierarchy favors conversation and action; existing components and state are reused; unsupported feature work is deferred; resize, focus, failure, and permissions are specified. Final precision and real-window acceptance remain implementation gates.

## Development inspection record

The first fresh dev build failed amid concurrent working-tree changes with missing `CompactionProcedure` and `NavigationProcedure` imports in `threadlane-runtime/src/harness/agent.rs`. Those files were not edited by this design task. An existing binary dated September 16 was packaged in `/tmp` for a fallback attempt, but Computer Use did not obtain a window from that bundle.

The second build through `scripts/run-gpui-macos.sh` succeeded in 5m 12s after concurrent source changes progressed. Launch then terminated with signal 6 after a `com.apple.hiservices-xpcservice` connection error. Attempts to open the freshly built bundle through Computer Use, both by absolute path and by `dev.threadlane.app.dev`, timed out. This establishes successful compilation but does not establish a working dev window or the root cause of the launch failure.

Those launch failures were the initial inspection result. During implementation, a freshly compiled executable was copied into `/private/tmp/threadlane-ui-goal/Threadlane-UI.app`, compared byte-for-byte before signing, ad-hoc signed, verified, and opened with Computer Use under the distinct bundle identifier `dev.threadlane.uigoal`. The installed application was not running. Development settings and Review were subsequently inspected.

Implementation evidence so far:

- The compact composer, Tools menu, quieter task rows, branch context, scoped changes summary, compact footer, and side-by-side Review were observed in the development window. Native resize refreshed screenshots that had remained on the initial frame; accessibility updates were available before the image refreshed. The latest 1440×900 build displayed the diff and conversation correctly. Subsequent native activity expansion exposed a crash caused by applying a custom hover style to a kit Button that already owns its hover style. A new GPUI regression reproduced that exact panic; the redundant style was removed. Earlier capture delays are not all attributed to this failure.
- A narrow sidebar geometry regression verifies the title and Needs you status fit at 223 logical pixels, and retains pointer, Enter, Space, archive, and settings activation checks.
- The six affected UI/state packages passed 161 tests after the permission and alignment changes. History regression verifies new activity preserves an older reading position and Jump to latest resumes following; plan regression verifies long-label bounds, click disclosure, and isolation after task switching. Permission regression verifies that details for an earlier request cannot apply to its replacement. After the activity-hover regression was added and fixed, the final rerun passed all 162 tests; cargo check and git diff --check passed.
- WCAG relative-luminance calculations on the existing theme values give body/background contrast of 17.36:1 light and 15.20:1 dark; muted text/sidebar contrast 4.78:1 and 5.18:1; primary button text/background contrast 9.11:1 and 6.52:1. No palette change was needed. This does not substitute for real-window focus/boundary and third-party-theme checks.
- A multiline Computer Use typing check treated a newline as Enter and submitted a test prompt. The turn was stopped; observed tool calls were read-only. Its task, “UI layout verification draft,” was preserved. Later input checks used paste or setValue, and the draft was cleared. Do not delete this task as part of UI cleanup.
- A disk-space failure was resolved by deleting only reproducible Cargo incremental output. No project, transcript, credential, or `.threadlane` state was removed.

Additional interaction evidence:

- A multiline unsent draft survived switching to another task and back, and was then cleared. Native Tab completion was verified with a matching prefix: /mod became /model followed by a space, without submitting. The draft was then cleared.
- Filtering to another project left the active conversation and composer project unchanged; clearing the filter restored the task list.
- Review opened a unified diff beside the conversation, returned to the file list, switched files, and closed. Cleared file selection remained cleared through refresh; the production path now uses the tested selection-retention helper.
- Light and dark settings cards, conversation, and Review were inspected at 1024×768; Dark was restored. The shared transcript/composer padding was subsequently aligned and inspected at 1280×800 and 1440×900.
- The final development build expanded completed activity and an individual tool result without crashing; both were observed in the native window. Temporary launch-size and panic-diagnostic changes were removed from the binary shell.
- Terminal ownership passed in the native app: two project shells displayed distinct harmless printf markers across task switches; hiding/reopening retained them; switching an unsent task to micontrol_engine showed its own fresh shell, and returning to the mypi task restored both original tabs and output. No project files were changed. The panel was left hidden.
- Native window zoom retained readable sidebar/review sizing. User-dragged split restoration requires a focused check; Computer Use drag attempts did not reliably grab the divider.

- After removing the temporary fixture, all 164 tests across the six affected UI/state packages passed (49.555s). cargo check, the normal development build, and git diff --check passed. The normal executable was copied, compared before signing, and its ad-hoc signature verified. Reopening it through Computer Use timed out twice; this does not establish a visual check of that final normal build.
- Controlled native acceptance used the existing session controller, permission handle, and question executor without model calls or executing an operation. Deny and Allow once resolved through their visible controls. A one-time request exposes no Always action, and the shared UI decision handler rejects stale IDs and unsupported persistent decisions. Permission details now reuse the kit Dialog; Escape dismisses it without cancelling the underlying turn. Regression coverage checks that precedence and closes stale request details.
- At 125% text size, permission details and question controls remained readable. A question accepted a selected option plus custom text. Dismiss discarded an unsent draft; a second question with the same item ID opened with an empty custom field and accepted Yes. The missing-worktree fixture retained its transcript and displayed Not checked out / Worktree unavailable. This seeded fixture does not establish durable restore or canonical-project PR lookup.
- A prior live-agent question check did not follow its instruction to avoid file changes and wrote a verification note. That note was preserved outside the repository at /private/tmp/threadlane-ui-goal/unexpected-agent-note.md and was not used as acceptance evidence. Subsequent checks used the controlled fixture. Temporary fixture code, dependency, and font override were removed.

Still required: complete keyboard/overlay checks; user-dragged split restoration; scroll-position precision during large activity expansion; durable missing-worktree restore and PR-context journey. Automated state regressions do not establish completion of those native journeys. The goal remains active.

### Startup investigation

A live one-second sample of the normal development process (PID 27385, 2026-09-17 14:06 EDT) locates the launch wait on the main UI thread: WorkspaceView::build → AppState::load_from_registry → discover_session_stubs_in_project → read_dir → __open_nocancel. The process remained live and was not restarted merely because Computer Use timed out. Evidence is preserved at /private/tmp/threadlane-ui-goal/launch-sample.txt. Initial stub discovery currently performs filesystem reads before constructing the window; the existing background refresh cannot unblock that initial read. A responsive startup path must preserve remembered session selection and hydration while removing this blocking dependency. This remains unresolved and prevents final native acceptance of the normal build.

### Overlay keybinding follow-up

A native PopupMenu regression reproduced the same Escape precedence failure as the permission dialog. The stop binding is now scoped to ThreadlaneWorkspace at the workspace root instead of being global with a Dialog exception. The actual kit menu receives Cancel first, dismisses, and restores focus; a second Escape then reaches the workspace stop action. All seven workspace tests pass independently, including the existing dialog check. The workspace explicitly enables its existing GPUI dependency's test-support feature for tests, removing reliance on another selected package enabling it. cargo check and git diff --check passed. This is GPUI interaction-test evidence, not a native-window verification of every overlay. The startup filesystem wait remains unresolved.

### Responsive startup implementation

StartupView now opens immediately and runs the unchanged AppState loader on a named OS worker thread. The existing WorkspaceView receives that loaded state, preserving session choice, canonical transcript selection, and pending hydration. A failed worker renders a named error and retry control; pending loading has a status role. A GPUI regression proves the window renders while the load channel is pending and reaches the failure state if the worker disconnects. All eight workspace tests pass independently; cargo check and git diff --check pass.

The native development window visibly displayed Loading projects… and responded to zoom while discovery was still pending. This verifies responsiveness, not successful project loading. macOS diagnostics also reported that the temporary bundle was absent from Launch Services; registering the same bundle succeeded. The running window still awaited discovery at the last inspection, and a relaunch after registration plus successful transition to the real workspace remain to be verified. The status accessibility additions were tested but have not yet been repackaged into the native bundle.

### Standard development bundle and palette focus

The current executable was packaged at the repository's standard target/debug/Threadlane-dev.app path with dev.threadlane.app.dev, compared before signing, signed, and verified. Computer Use observed the named Loading projects status. After a prolonged system delay, the real workspace loaded and restored UI layout verification draft, its persisted transcript, and the Review panel. Successful loader-to-workspace transition is therefore now observed. The original directory wait's external cause is not established; no macOS privacy permission was changed. A request to open System Settings took about 68 minutes inside Computer Use, after which the tool returned. The earlier outstanding question about a visible macOS prompt is no longer necessary to continue native checks.

The native Review divider now demonstrably responds to drag. Its custom width narrows at the smaller window size and expands on return to the maximized window. Exact width retention and hidden-panel restoration remain unverified. Cmd+K opened the command palette and Escape removed it. Subsequent shortcuts stopped responding: source inspection confirmed dismissal hid the focused input without restoring its previous focus. All dismissal paths now share a focus-restoration function; toolbar and keyboard opening share the same handler, and clicking inside the palette stops propagation to its dismissal backdrop. A GPUI regression verifies focus returns once and a repeated dismissal cannot restore a stale focus handle. All nine workspace tests, cargo check, and git diff --check pass. The native running bundle predates this last palette-focus change and must be rebuilt/reopened for final verification.

### Latest verification

The palette-focus build was compiled, copied into the standard development bundle, compared before signing, and signed/verified. Its startup again remained at Loading projects during the immediate native check, so the palette fix is not yet manually verified. The six affected UI/state packages passed all 167 tests in 48.067s. The unnamed native Review button was traced to the PR external-link control; it now has an explicit accessible name. The branch-manager back button and three Git-dialog close buttons also have names, with missing close tooltips supplied. cargo check and git diff --check pass after those label changes. No Git action was executed.

### Durable restore acceptance

An isolated project under /private/tmp/threadlane-ui-goal/restore-fixture used a local shared clone with its own index and disabled hooks. The temporary native fixture wrote 40 real JsonlStore messages plus missing-worktree and recorded-branch facts, then invoked the production load_from_registry, discovery, hydration, and PR lookup paths. Computer Use observed restored historical responses through response 40, Not checked out in the task row, Worktree unavailable in the composer, and PR #219 resolved through the canonical project's remote. The Review surface withheld unavailable worktree actions and offered Use project folder. No model request or Git mutation was executed through the app. This completes the representative missing-worktree restore/PR-context journey.

In that same native run, Cmd+K opened the command palette, Escape closed it, and Cmd+R immediately opened the right panel, verifying the palette focus fix. Pointer activation opened Tools with Editor and Trajectory; Escape dismissed it. Exact divider restoration and comprehensive keyboard/scroll acceptance remain pending. Temporary fixture source was removed after verification; durable fixture records stay outside the repository as evidence.

### Scroll precision regression

The existing completed-activity regression now places a tool result among 60 historical messages, positions the reader at that message, opens the group, and opens a 250-line result through real GPUI pointer events. Both expansion steps preserve the exact logical top (message index and pixel offset), and tail following remains paused. The focused test passes. Combined with the earlier new-activity/Jump-to-latest regression, this proves both state transitions numerically; it does not substitute for the remaining native long-output interaction check. The tool disclosure gained only a test selector. The ordinary development process is still showing Loading projects at this inspection; it was not restarted.

### Native history, splits, and Settings keyboard follow-up

An isolated durable transcript included a completed 250-line tool result. Native expansion and collapse retained the same response 38, checkpoint 39, response 40, and tool heading at the same vertical positions; Jump to latest returned to checkpoint 16. Closing/reopening Review retained the historical conversation position. A dragged Review split at the half-window limit retained its split when hidden and reopened, and after shrinking, hiding, maximizing, and reopening. Arbitrary intermediate-width pixel precision was not measured.

Keyboard checks verified Cmd+K / Escape / subsequent shortcuts, composer Tab/Space into the model menu, Escape then Tab/Space into effort, and Shift+Tab from composer into Tools. Tools reopened with Space after Escape. Settings initially left focus on the hidden composer. The workspace now establishes a tab group and transfers focus on Settings page transitions; native Tab/Space selected Appearance, Shift+Tab/Space returned to General, eight Tabs/Space activated Back, and focus returned to the composer. All five settings switch sites now have explicit accessible names; the General switch name was observed in the native accessibility tree without toggling the setting.

Temporary startup and AppState fixture wiring was removed; the user project registry was checked and contains no fixture entry. The fixture transcript is retained outside the repository. The final exhaustive accessibility/design audit remains outstanding; these checks do not claim every product control or third-party theme was exercised.

Final cleanup validation: all 167 affected UI/state tests passed; after narrowing focus transfer to Settings transitions, all nine workspace tests passed again. cargo check, cargo build, and git diff --check passed. Both development bundles now contain the normal executable, compared before signing and signature-verified. Computer Use opened the standard development bundle and observed Loading projects…; final normal-workspace and exhaustive accessibility acceptance are still pending.

### Accessibility contract audit

The source audit found unknown context usage still exposed a zero-valued progress circle despite having no percentage. Unknown native usage now reads Unavailable; the circle and bar render only with an actual measurement. ACP retains its explicit Not reported explanation. Existing missing/estimating/zero-limit context tests were updated and all 49 chat tests passed.

Six remaining icon-only buttons gained accessible names: copy/close trajectory, dismiss update notice, push commits, remove extension, and remove ACP agent. A scan of Button builders in the five changed screen owners found no further icon-only buttons missing a name or tooltip; dynamic wrappers and other control types still require native inspection. The startup screen now establishes a tab group, and its existing pending/failure regression additionally verifies that Retry can receive keyboard focus. That focused test passed.

The normal development process PID 55480 remains live. A fresh one-second sample again locates the wait in workspace-startup → load_from_registry → discover_session_stubs_in_project → read_dir → __open_nocancel. The native window still displays Loading projects. Evidence: /private/tmp/threadlane-ui-goal/final-startup-sample.txt. The process has not been restarted to work around this wait. A user question about a possible macOS file-access prompt is pending. Final native audit of the normal workspace is not complete.

### Normal workspace audit and follow-up fixes

PID 55480 subsequently completed startup without a restart, restored the real UI layout verification draft transcript, and exposed the actual repository/PR. Review opened context_meter.rs, refreshed, and closed with the conversation retained. The permission-prompt question is no longer needed for that launch.

The real transcript revealed a pointer-only reasoning header. It now uses a named native Button; its regression exercises pointer expansion, tail pause in overflowing content, Tab traversal, and Space collapse. The test passes. The native kit deliberately avoids focusing buttons on pointer down, so keyboard verification explicitly traverses to the control instead of assuming pointer activation sets focus. Review branch selection and Changes/History tabs were likewise converted from clickable containers to selected native Buttons.

A non-clamped split reproduced the remaining resize defect: at 1100px the Review boundary was dragged from 688px to 770px (330px panel); hiding Review, maximizing, and reopening grew it substantially. Returning to the original window restored approximately the preferred width. The root cause is restoring hidden resizable state against stale container bounds. Restoration now follows a viewport/rem/visible-panel signature, waits for the next frame, skips hidden panels, and discards superseded layout callbacks. It reuses the same native resize API and existing drag-owned preferences. Native verification of this fix is still required.

The new normal binary was copied into both development bundles, compared before signing, and signature-verified. The previous workspace was closed only to install these changes. The new normal process currently displays Loading projects. The startup keyboard group now has an explicit accessible role/name; this final role addition awaits native verification. No temporary fixture source was reintroduced.

### Split and keyboard verification in the normal app

The updated normal app restored the real workspace. A Review divider at x760 in a 1100px window produced a 340px panel. After hiding Review, maximizing, and reopening, the panel remained approximately 340 logical pixels (275 rendered pixels at the screenshot scale); returning to 1100px restored the boundary exactly to x760. This verifies the previously failing hidden-panel path.

An unsent new task showed Unavailable in the context popover and no progress-indicator node, rather than implying 0% current usage. The original task was restored without submitting a prompt. Five Shift+Tabs from the composer reached its reasoning disclosure; Space expanded it and a second Space collapsed it, with a visible focus ring and correct accessible labels.

The native button conversion exposed centered reasoning/branch labels and clipped Changes/History text. The content flex ownership was corrected and extra tab vertical padding removed; cargo check/build and whitespace checks pass. These visual corrections are compiled but the currently running bundle predates them.

Remaining audit findings are explicit: the legacy aggregate-tool disclosure in chat and secondary Git stash/history/branch/merge/switch-choice controls still use clickable divs. They must gain keyboard-operable native controls without changing Git safety semantics. Final native visual checks and the design-guide completion audit follow those corrections.

### Secondary controls and native Git dialogs

The remaining aggregate-tool, progress-summary, and trajectory disclosures now use named native Buttons. The redundant clickable permission-preview text was removed; the existing Details button remains the entry point. Nine secondary Git stash/history/branch/merge/switch controls now use native Buttons while retaining their existing callbacks and selection state. Git form/filter inputs have accessible names.

Create, merge, and switch dialogs now reuse the kit Dialog for focus, Escape, and dismissal. Explicit action buttons retain Git execution; opening a dialog or pressing Enter does not execute an operation. A GPUI regression opens all three dialogs, traverses the switch choices with Tab, activates Carry with Space, verifies Git remains idle, and checks Escape clears dialog state and restores trigger focus. Test windows omit the unrelated Wry browser because they have no native window handle; production construction is unchanged.

All 169 tests across the six affected UI/state packages passed in 42.058 seconds. The normal executable containing these changes was packaged and signature-verified in both development bundles. Computer Use observed the named Workspace startup / Loading projects accessibility nodes in the standard bundle. The final dialog methods were formatted without changing behavior; whitespace validation passed.

The final native control check is still pending: the latest normal window remains at Loading projects, including after attempting the workspace command-palette shortcut. No Git operation was performed and the process was not restarted to work around this wait. After the same blocker persisted across three consecutive goal turns, the goal was marked blocked rather than complete. A fresh sample of live PID 13664 locates the startup worker in load_from_registry → discover_session_stubs_in_project → read_dir → __open_nocancel; the UI thread remains responsive. Evidence is preserved at /private/tmp/threadlane-ui-goal/current-startup-sample.txt. Resume final native dialog, secondary-control, and design acceptance when project discovery completes or the underlying filesystem-access wait is resolved. No macOS permission change is inferred or requested from this stack alone.

## Final acceptance

The same normal process subsequently completed discovery without a restart, restored the real task and transcript, and allowed the remaining native checks. This supersedes the pending/blocked entries above. The directory-read delay's cause remains undetermined; responsive startup does not eliminate that external wait.

| Requirement | Acceptance evidence |
| --- | --- |
| 1. Composer, scope, autocomplete, draft | Native multiline draft survived task navigation; /mod completed to /model without submission; project/mode/model remained visible. |
| 2. Task filtering | Native project filter changed only the presented list and clearing it restored the list without changing the active conversation or composer scope. |
| 3. History and activity | Native long-result expansion/collapse and Jump to latest checked; GPUI tests assert exact logical message/pixel offset for expansion and incoming activity. Final normal build expanded completed activity and retained readable reasoning/tool rows. |
| 4. Review | Native file selection, unified diff, refresh, close, conversation retention, and arbitrary 340px split restoration checked. Final Changes/History labels and branch header render without the prior clipping/centering defects. |
| 5. Permission and question | Controlled native production-manager flow checked explicit decisions, one-time scope, selection/custom answers, dismissal, and fresh-request drafts. GPUI regressions cover stale IDs, unsupported Always, and overlay Escape precedence. |
| 6. Terminal ownership | Native independent shell markers survived same-project task changes and hiding; changing projects selected the other terminal group. |
| 7. Missing worktree | Isolated durable JSONL fixture loaded through production discovery/hydration; native transcript, unavailable-worktree state, and canonical-project PR #219 observed. Fixture wiring was removed. |
| 8. Keyboard, overlays, constraints | Native sidebar, composer/model/effort, Tools, palette, Settings, reasoning, Git dialogs, history, and Trajectory paths checked alongside GPUI interaction regressions. Light/dark, 125% text, long labels, and target window sizes inspected as recorded above. |

In the final native pass, Create branch accepted keyboard focus and Escape dismissed it. Merge accepted Tab/Space selection, exposed the selected candidate in its accessible state and commitment label, and dismissed with Escape. Switch accepted Tab/Space selection of Carry with a visible focus ring, then dismissed with Escape. No create, merge, checkout, stash, commit, or push was executed. History's first commit and first file opened through Tab/Space; the historical diff rendered read-only. Trajectory's first row opened its inspector through Tab/Space with a visible focus ring. The diff and inspector were closed and the original conversation restored.

The history-to-editor journey exposed an unnamed close icon. It now has `accessibility_label("Close tab")` alongside its existing tooltip. This final one-line accessibility change was compile/build verified; its new spoken label was not separately observed in a restarted native process. Both bundles were updated atomically, their executable hashes compared before signing, and their ad-hoc signatures verified.

### Design-guide audit

- **Task and action:** title, project, execution location, branch, composer, current state, and scoped Review entry remain visible; draft/filter/terminal ownership is preserved.
- **Action promises:** workspace changes are labeled as such; context usage can be unavailable; Git dialogs separate selection from execution; request IDs bind permission decisions.
- **Hierarchy and restraint:** transcript/composer own the central column; routine metadata is quiet; completed activity and diagnostics disclose on demand; no extra environment column or settings search was added.
- **Geometry:** shared rem constraints and content insets govern the layout; measured split restoration and focused bounds/scroll tests complement native checks at the target sizes. No isolated alignment offsets were introduced to fix the observed button defects.
- **Components:** native Buttons, menus, Popover, Dialog, inputs, and resizable state provide the changed interactions. Scoped slash completion retains its existing keyboard action path. No production dependency, new framework, or parallel project/session state store was added.
- **States and accessibility:** loading, empty, failure, permission, question, unavailable-worktree, selection, and unknown-usage paths have explicit presentation. Changed controls use accessible names/roles and native keyboard contracts; semantic theme colors and measured text contrast are retained.
- **Real-window evidence:** acceptance used the actual development app plus isolated controlled fixtures where executing real permissions or missing-worktree flows against user state would be inappropriate. Accessibility trees were inspected; VoiceOver audio and arbitrary third-party themes were not certified. Existing Git mutation semantics were preserved rather than exercised against this working tree.

Final validation: 169/169 tests across the six affected UI/state packages; `cargo check -p threadlane-gpui`, `cargo build -p threadlane-gpui`, and `git diff --check` pass. The last editor label change did not alter interaction logic. No temporary fixture remains in the production startup or AppState paths. Changes are uncommitted for review.
