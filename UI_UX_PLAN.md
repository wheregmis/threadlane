# Threadlane UI/UX plan — lessons from Synara

Prepared 2026-09-17. Scope: design and implementation plan, with no product-code changes.

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

Consequently, the live Threadlane comparison is limited to the installed app's conversation/workspace. Threadlane settings and auxiliary-panel proposals are source-informed rather than visually verified in the dev app. Re-run the listed journeys against the dev build before implementing or approving final geometry. The plan is ready for review; development-app visual verification remains blocked by launch. `git diff --check` passed; no product source was changed by this task.
