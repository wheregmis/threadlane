# Threadlane UI Kit preview

The `@` file picker uses the production kit menu, query matching, keyboard
bindings, and path insertion. Session imports capture the checkout's Git file
inventory for local draft editing on native and web. The Components gallery
includes loading, empty, failed/retry, unavailable, capped, and long-path samples.
Preview Retry uses captured data; import again to refresh actual workspace files.

Conversation code blocks use the production kit frame and Markdown parser. The
Components gallery includes completed shell/Rust and streaming samples with long
file paths. Imported snapshots record desktop shell availability; preview
Run/Open actions show local notices and preserve the captured conversation.
The gallery's tool disclosures use the production file, search, and command
renderers, including source line numbers, match highlights, exit status, and
running/error states. Short results fit their content; long output stays within
the shared scroll limit. Long source and command lines scroll horizontally inside
the output region while the header stays fixed. Gallery file actions show a local notice.
Output cards include the same accessible Copy action on desktop and web. Source
copies omit line-number anchors and snapshot notices; command copies include
stdout and stderr. Copy feedback resets when streaming output changes.
On macOS browsers, scoped Command shortcuts support Select All, Copy, Cut,
Paste, Undo and Redo alongside the existing Control bindings.

The default view is a saved-session workspace (`session.rs`). It composes the
production components from `threadlane-ui-kit`: sidebar/navigation, session
metadata, workspace split, chat header, virtual transcript, tool result cards,
composer and its context chips, prompt rail and outline, message copy/edit controls,
conversation find, last-run duration, plan tracker, context disclosure, environment panel
and token accounting. The **Components** button opens the interactive gallery (`gallery.rs`).
Both views run unchanged on native GPUI and GPUI Web. The workspace’s **Editor**
tab opens sample buffers using the production editor components. The gallery also shows the
same saved-session environment panel at narrow widths.

Hosts own fixture state and callbacks. Editing a draft or expanding tools stays
local. The rail and keyboard outline jump to canonical prompt identities; Up/Down
recall follows the desktop caret and draft guards. Copy uses the local clipboard;
Edit loads the composer only when its draft is empty. The shared transcript frame
provides the same padding, scrollbar and jump-to-latest control as desktop.
Desktop-only services (sending prompts, opening files, discovery and
settings persistence) are not run by the preview. Navigation destinations still
being extracted show an explicit preview notice.

## Import a conversation

From the repository root:

```sh
cargo run -p threadlane-ui-kit-preview -- --import-session /absolute/path/to/.threadlane/sessions/session.jsonl
```

This reads the canonical durable transcript and metadata without restoring or
modifying the session. Archived transcripts can also be imported. Metadata-only
stubs are rejected. It creates ignored `session.local.json`; the next build embeds
that snapshot on both platforms. A clean checkout uses `session.sample.json`.
Keep private snapshots and generated `dist/` local; they contain conversation data.
The snapshot also includes the saved model and effort, current project mode and
active skill count, plan, context usage, last-run timing, canonical trajectory events, token accounting and the
checkout’s current Git inspection. Missing saved model metadata is shown as
unavailable rather than replaced with an invented selection.
It does not fetch, pull, push, or write repository changes. The captured date is
part of the fixture so native and web show the same groups.

## Native

The public sample includes two captured Review diffs. Open **Review**, then a file
or **All changes**, and use **Find in diff** (Cmd/Ctrl+F). `LEGACY_TIMEOUT` finds
deleted text, `CAFÉ` exercises Unicode case folding, `removed_offscreen` reveals
the end of the long patch, and `not-in-this-patch` exercises the no-match state.
Previous/Next and Shift+Enter/Enter wrap within the loaded patch. The sample and
imported patches are read-only; searching never loads files or runs Git.

From the repository root, using the app's Rust toolchain:

```sh
cargo run -p threadlane-ui-kit-preview
```

The native preview supports Cmd+Q and the macOS Quit menu for rebuilding between checks.

## Web

Install [Trunk](https://trunkrs.dev/) once (`cargo install trunk --locked`). From
this folder, install the pinned web toolchain and serve:

```sh
rustup toolchain install nightly-2026-06-18 --profile minimal --component rust-src --target wasm32-unknown-unknown
trunk serve
```

Open <http://127.0.0.1:8080>. `trunk build` creates `dist/` for a static preview.
Trunk watches the gallery, kit source, and shared theme source; edits rebuild
and reload the page. The native app's Rust 1.95 toolchain stays unchanged.

The [Threadlane website](../../../../website/README.md) embeds this preview and
deploys it to GitHub Pages. Its build sets `THREADLANE_PREVIEW_PUBLIC_SAMPLE=1`
to force the checked-in sample instead of any private `session.local.json`.

The setup follows Zed's [GPUI Web hello_web example](https://github.com/zed-industries/zed/tree/main/crates/gpui_web/examples/hello_web):
one canvas window, a retained `ApplicationHandle`, embedded fonts, WASM atomics,
rebuilt standard library, and COOP/COEP response headers. It uses Threadlane's
existing pinned GPUI backend. WebGPU is preferred with a WebGL fallback.
The preview uses GPUI's single-threaded web host, matching GPUI Kit's gallery.
This avoids Markdown channel contention calling `Atomics.wait` on the browser's
main thread when opening components. Parsing still uses the shared Markdown
implementation; large fixtures may take longer than the native worker path.
Serve over localhost or HTTPS, with the headers in `Trunk.toml`; opening
`index.html` as a local file does not provide the required isolation.

The web theme uses bundled defaults; it does not load or persist host settings.
Browser accessibility and platform behavior remain bounded by GPUI Web's current
canvas backend. Native checks are still required for desktop changes.
The pinned GPUI Kit omits tree-sitter grammars on WASM, so source, shell and
Markdown code currently appear without syntax colors in web previews. Native
syntax highlighting remains intact; diff and search highlights are portable.
The [browser highlighting probe](experiments/wasm-highlighting/README.md) records
the tested parser path and the dependency changes still needed for parity.

## Fonts

Shared theme initialization registers IBM Plex Sans in regular, medium, semibold
and bold, with matching italics, plus regular JetBrains Mono on native, iOS and
web. Using real font faces keeps hierarchy consistent without platform-specific
synthetic styling. Font provenance and pinned revisions are in the
[font inventory](../../../threadlane-ui-theme/assets/fonts/README.md); the files
are covered by the [SIL Open Font License](../../../threadlane-ui-theme/assets/fonts/OFL.txt).
Custom native themes can still override the font families. The Components view
includes matching upright and italic specimens at all four UI weights.
Both bundled themes explicitly use a 16px UI base and 13px monospace base;
code chrome and identifiers use the theme's monospace family as well.
Compare native and web at 100% browser zoom: browser zoom scales the whole
canvas independently of GPUI's shared font size and rem geometry.

## Extraction coverage

Components starts with up to eight real imported trajectory events. Their row, grouping-header, overview, statistics and readable-output atoms are shared with desktop. Select a row to show its captured output. Summary counts cover the complete captured trajectory. The workspace Trajectory panel also uses the complete shared `TrajectoryView`: Execution, Requests, Model Context, Durable Events and Recovery, category/lane filters, search, virtual list and inspector tabs. Imports capture each mode through the same canonical diagnostic projectors as desktop; older snapshots without these fields show empty diagnostic modes until reimported.

The Agents preview now mounts the production right-panel title, surface tabs and
chooser. Navigation retains the selected agent and expanded tools. The Browser tab
uses the production toolbar, address input, scrolling tab strip, annotation hint
and page viewport. Tab selection, creation, closing and annotation cancellation
run locally; address edits never send network requests. Native page rendering and
annotation picking stay in the desktop webview host. The Review tab uses the shared
workspace context, branch and sync controls, Changes/History tabs, filter, selection
toolbar, list/tree rows, commit footer and diff presentation. Import captures bounded
read-only diffs from the session's checkout, including both whitespace settings.
Selection, filters, folder expansion, drafts and opening diffs run locally. Git actions
show a notice and never modify the checkout. History and current-stash cards, file rows,
filtering, expansion and empty/loading states use the production components. Import
captures their file lists and patches read-only within a shared 5 MiB budget. Missing
patches show an explicit notice. Recorded diffs open in the existing Editor tabs and
preserve unsaved sample drafts. Expanded review details scroll above the commit controls
on short panels. The compact PR card and selection-aware file/discard menus now use
the production components. The import captures the canonical actionable-feedback
count and the desktop file-manager label. Copy commands copy the displayed path, and
Open Diff in Editor Tab opens the captured patch. Other file/PR actions show a local
notice without modifying the checkout, opening external pages or starting a task.
Components includes labeled failing, pending, passed, absent and busy PR check states.
Branch management uses the production searchable groups, current-branch marks and context menus.
Create, merge, dirty-checkout switch and stash dialogs share the same forms and modal shell
as desktop, including keyboard controls, dismissal and busy states. Branch-name copying
works locally; deletion confirmation and form submits only show a notice. Recorded Git
status, selected files and drafts remain intact. Draft-PR fields, generation controls, status
feedback and dialog shell now reuse the production components too. The recorded Create draft
PR button opens a retained local form when available. Components provides Ready, Failure,
Uncertain, Changed checkout and Newer edits samples; generation, creation and readback use
local timers, preserve newer edits and never request a model or GitHub. Live Git errors and
other panel bodies still need extraction. The Files
tab uses the production tree rows, expansion and context menus, with a labeled sample
file that opens the existing local editor and preserves its draft. The Components
gallery exposes the production panel document header and whitespace control. File
access, search, save guards and live diff loading remain in the desktop host. The footer’s
Agents control opens and closes the preview panel.

Sidebar/navigation and session identity, workspace split, chat header and tabs,
composer frame and control appearance, virtual transcript, Markdown, reasoning,
activity disclosures, and command/file/search/directory/diff cards are shared.
Environment, Git menu presentation, token-efficiency rows, the complete context
disclosure, composer context chips, and the complete plan tracker are also shared. The environment uses the same available-width
breakpoint as desktop and hides when the conversation needs the space.
Feature hosts retain domain routing and file-path guards.
The queued-message panel and its controls are shared. The gallery includes
local queue editing, supported/unsupported steering, and pending removal with
an explicit sample acknowledgement. The saved-session workspace only shows
real captured session content; it does not invent queued messages.
Pending follow-ups and staged-image chips use the desktop components. A generated
checkerboard fixture exercises the same Fit/Actual size image-preview layout
without bundling private attachments. Desktop retains upload data and safe decoding.
Saved-draft banners, save/restore/discard controls, the discard confirmation, and
prompt-recall strips are shared too. The gallery keeps draft changes local and
recalls user prompts from the imported session; it never writes to saved sessions.
The composer uses the production searchable model popup and project menu. Its
model row is the captured session model, not a live provider catalog. Search,
empty-state recovery, grouping and checkmarks use the same picker components as
desktop. Project attachment remains a local preview notice.
The sidebar uses the production project-filter surface and menu rows, including
session counts and checked choices. Selecting the captured project or All projects
updates local filter state without switching the chat. Its count reflects the
one imported session.
The saved session also uses the production pin/archive/actions buttons, compact
actions menu and full right-click menu. Pin and snooze changes stay in preview
memory; return-time choices are captured by the native import adapter from the
canonical sidebar helpers. Copy commands use captured identity paths, and Open
terminal shows the local terminal sample. Title generation, forks and exports show a local notice. Desktop keeps its existing
services behind the same typed menu commands.
The complete session card now uses the same production renderer: recency/action
slot, context row, Git/PR metadata, pin/result/attention signals and snooze badge.
Pinning changes the local group; confirmed snoozes use the shared collapsible
Snoozed header. Archive and Remove open the shared object-specific confirmation
dialog, including the controlled worktree choice when applicable. Confirmation
hides only the local preview row and reveals the production empty-history
surface; **Restore preview session** in the labeled preview footer brings it
back. No session journal or worktree is changed. Components includes labeled
Ready, Working, failed-snooze and unavailable-worktree card samples.

The Editor tab reuses production tabs, their context menu, save/status controls,
buffer and diff viewports, and the empty state. Its labeled sample file can be
edited and saved locally with the button or Cmd/Ctrl+S; the sample diff is read-only.
Close controls are separate from selection, and unsaved sample edits require
confirmation. Reset editor samples restores the demonstration. Preview buffers
never read or write project files. Desktop retains file I/O, syntax configuration,
and its OS unsaved-close confirmations.

The Automations sidebar entry reuses the production list, detail toolbar, prompt
disclosure, scope menu, attention filters, run rows and pagination. Its labeled
in-memory samples support pause/resume, review, cancel and history removal.
Run now adds only a queued sample row; no scheduler or provider is started.
Sample project paths retain their rooted POSIX form on WASM so canonical validation
can accept the captured project without accessing its filesystem.
Sample runs have no associated chats, so Open chat stays disabled. New/Edit use
the production form and sheet, including conditional schedule fields, next-run
preview, validation, keyboard save and Escape cancellation. Save changes only the
in-memory sample definitions; it never creates a run. Samples use one static
model and project catalog and a fixed clock instant. Returning to the saved
session preserves its transcript and selects Chat.

Settings opens the shared full-window navigation, General, Appearance, Shortcuts,
Agent & Fusion, Skills, WASI Extensions, ACP Agents and Providers pages. General uses local project/update/preference samples; update checks and PR
review toggles never invoke services. Theme selection changes the preview process
only, without saving the desktop preference. Back returns to the previous workspace
surface with its transcript and local state preserved. Agent & Fusion has local sample
model, reasoning-effort and mode menus; selecting a model without reasoning hides
the effort field. These choices never write project preferences or rebuild a runtime.
Skills include enabled, disabled and invalid samples. Extensions include project
and global copies of one module to exercise overriding and independent controls.
Refresh, Disable all, scope, toggle, remove and Install .wasm operate on local
sample records only; no files are selected, installed or removed.
ACP Agents uses the production scope controls, preset rows and labeled custom-agent
form. Local samples exercise separate global/project records with the same ID,
enabled/disabled/error states, adding, toggling and removing agents. Refresh never
starts an agent process, and changes never write ACP configuration.
Providers uses the production connection rows, account controls, masked key fields
and status feedback. Sample sign-in, account switching, disconnect, Save and Test
actions remain local; no credentials are loaded, changed or sent over the network.
All settings destinations now reuse the shared presentation.

The Issues and PRs sidebar entries now open the production collection toolbar,
project-scope menu, search and state filters, virtualized issue/PR rows, pagination,
empty/loading/error states, master–detail split and status bar. Labeled local
samples include duplicate item numbers across projects, draft PRs and failed
checks. Arrow navigation, selection, scope, filtering, search, Load more and
recovery stay local; the preview never queries GitHub or creates an issue.
The Preview state menu exercises refresh, partial failure, complete failure and
no-project states, detail refresh/error recovery, and pending issue actions.
Issue headers, actions, Markdown descriptions, linked tasks and virtualized
comments now reuse production components. PR headers, keyboard tabs, overview
descriptions, review badges and check rows are shared too. Tab selection is
retained independently for each project and PR. Preview issue actions change
only sample state or show a local notice; they never start tasks or modify GitHub.
PR discussions reuse production comment/review rows, inline reply controls,
draft editors and publication feedback. Draft text and reply targets remain local
and survive switching projects or PRs. Publishing, failure and uncertain-outcome
samples exercise the same presentation; Post, Retry and Check again never send
anything to GitHub. Commit rows and their virtual list are shared, including
long messages, zoom, scrolling and empty lists.
The Files changed tab now reuses the production file rows, keyboard list,
viewed-status toolbar and selectable diff surface. Its 24-file local sample covers
long and Unicode paths, renamed/deleted files and binary changes. Up/Down/Enter,
Viewed, Next unviewed and Retry diff run against scoped sample state. The state
menu also covers diff loading/failure and viewed loading/failure/pending/uncertain
states. GitHub requests, viewed-write guards and canonical diff preparation remain
in the desktop feature; the preview does not perform remote writes.

New issue uses the production form and dialog, with the same labels, validation,
focus, busy controls and inline errors. Select a project, then choose New issue.
The local request takes one second; the “Issue creation fails once” preview state
keeps both fields intact through a failed request and allows a successful retry.
Success displays a local notice without creating a GitHub issue. Desktop retains
the same draft until GitHub acknowledges creation instead of closing on submit.

Start-task and delete confirmations also reuse production presentation. Start
shows grouped models, optional reasoning, mode, and the canonical branch preview;
choices stay local. “Task start fails once,” “Task requires Git repository,” and
“Task needs provider” exercise error recovery and unavailable actions. Pending
starts block duplicate submission and keep the form mounted. Delete names the
selected repository/project and issue; bare Enter never confirms it. Its callback
only shows a local notice, and changing selection invalidates confirmation. The
desktop callback likewise checks the original project and issue before mutation.

The saved workspace's **Terminal sample** button opens the shared resizable bottom
panel. The gallery also includes terminal chrome. Tabs, their context menu, the
toolbar, and unavailable-worktree recovery use production components. Shell tabs
retain identity when reordered; output-bearing shells require a second close
activation. Only the tab strip scrolls horizontally. New tab and Hide stay visible,
and secondary actions wrap at narrow widths. Sample selection adds a labeled local
excerpt to the preview draft; nothing is sent. Clear, Restart and recovery modify
only local fixtures. No PTY is spawned and no shell command runs.

Find uses the production strip, status copy, case-sensitive matching, navigation
order and focus-scoped shortcuts. Queries and results belong to each sample shell.
The **Sample search state** menu exposes searching, failed/retry and full-screen
states. The link picker uses the production frozen, deduplicated menu and returns
focus on Escape; choices are recorded locally and never open a browser. Desktop
retains its worker generations, retained-output scans and URL/epoch guards.

The terminal output uses the same `TerminalGrid` as desktop: ANSI colors, bold
spans, cursor, link underlines, selection and find cues. `TerminalTextMetrics`
measures the shared monospace font; `TerminalGridGeometry` uses the resolved
padding for resize and pointer mapping, including theme zoom. Dragging selects
actual fixture cells, and Add selection to chat inserts that excerpt locally.
Modifier-clicking a sample URL records a local choice without opening a browser.
Samples are parsed locally with the existing vt100 dependency; no PTY or worker
is spawned. Native frame identity, URL guards, search workers and I/O stay native.

Right-click output to use the shared `TerminalOutputMenu`: font sizes, compact
lines, blended background, Find, copy, selection, Clear and Restart. Clipboard
commands copy actual displayed text; Paste is disabled because the fixture has
no PTY. The **Output state** menu shows the shared retained-output/error banner;
its Restart action restores the local sample. Desktop also uses the kit's
live-output jump control, while scrollback and parser workers remain host-owned.

The workspace uses the same responsive geometry as desktop. Opening the right
panel replaces Environment; a tighter window hides the sidebar, then mounts the
selected right-panel surface on its own with **Back to conversation**. Returning
keeps the draft and selected agent. Widening restores the sidebar and saved panel
sizes. The shared sidebar control toggles local navigation when it fits; its
unavailable tooltip points to the command palette when the window is too narrow.
The preview footer uses shorter labels at compact widths so all controls fit.

The **Agents** control opens the same profile strip, status pills, instruction header,
activity cards and animated tool disclosures used by the desktop Agents panel.
The main profile uses the imported session. Import now includes durable child-lane
transcripts; older snapshots without children show labeled sample agents for
working, failed, completed and queued states. Message/Continue only fills the
local composer. Agent file previews are read-only and no run is dispatched.
Isolated agents also use the desktop worktree card: Inspect diff, Terminal, Apply
and Discard, with identical status/availability rules and confirmation copy.
Imports capture branch patches read-only within a shared 5 MiB agent-diff budget;
old snapshots have no branch patches or availability until reimported. Inspect
opens a captured patch; uncaptured/oversized patches explain how to reimport.
Terminal opens the local sample. Apply and confirmed Discard show local notices
and preserve all captured metadata. Labeled sample agents include worktree states
when the imported session has no child lanes.

The footer search control or Cmd+K/Ctrl+K opens the same command palette as desktop:
command labels, descriptions, shortcuts, settings destinations, session rows and
conversation-result rows all come from the kit. Search a settings alias such as
`theme`, then press Enter to open its local page. Search `conversations` to enter
saved-message search; its footer explicitly limits coverage to one captured
session. Confirming a hit validates message identity, then opens the shared inline
conversation find strip with that query and selected message. The chat header's
search button or Cmd+F/Ctrl+F opens the same strip. Previous/Next, Enter/Shift+Enter,
match status, selected-message excerpt and row highlight reuse desktop presentation.
Escape restores prior focus; Enter is scoped to the find input and never hijacks the
composer. Changing surfaces clears the search. The captured transcript is immutable;
no journal scan or write is performed by the web preview.
Escape and backdrop clicks dismiss the palette and restore prior focus. Terminal
selection remains disabled with a reason until text is selected in the visible
sample. Commands requiring host services show a local explanation without running
a command, writing settings, or changing session data.

Sidebar history collection/cache ownership and remaining host-specific panel chrome
still need auditing; this preview does not yet promise complete desktop screen parity.
