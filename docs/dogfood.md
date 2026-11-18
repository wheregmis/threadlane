# Dogfooding log

Work given to Threadlane itself, on the real hub (`.threadlane-dev/hub`,
space `dev`, project `threadlane`, real `claude`), and what it showed.
Newest run last. Each finding has a state: open, fixed (with the commit),
or accepted.

## How a run is set up

- The project has two repositories: `threadlane` (GitHub, branch `v2`,
  only as current as the last push) and `threadlane-local` (this checkout
  by path, branch `v2`, always current). Tasks are told to use
  `threadlane-local`.
- Tasks do not publish. Their work is a commit on `threadlane/T-xxxx` in
  `.threadlane-dev/hub/workspaces/worktrees/T-xxxx/<repository>`, fetched
  into this checkout by hand after review.

## Run 1, 2026-11-17: README status section (T-0001)

Goal: add a "Project status" section to README.md. Result: correct, 8
lines, one commit on the task branch. Not merged: later work overtook it.

| # | Finding | State |
|---|---|---|
| 1 | The lead told the owner the work "isn't committed yet". The runner commits at run end; agents were not told. | fixed in 415a8bf3 (instructions now say so) |
| 2 | The lead's final text is not delivered; only `send_message` is. A lead that just answers shows nothing. | open |

## Run 2, 2026-11-17: README update (T-0003) and gh pagination (T-0004)

Goal: two independent tasks, one documentation, one code with tests.

| # | Finding | State |
|---|---|---|
| 3 | The lead created both tasks but sent no message saying so; `threadlane send` waited 300 s and gave up. In run 1 it did reply. | fixed in 32910401 (lead instructions); not yet seen on a real run |
| 4 | The two tasks were asked to run at the same time; one ran. The Engineer's binding allows one run at a time by default, and nothing in the app or CLI says why the other waits. | fixed in b2c55262 (the reason on the card; "at once" per agent in the app and `threadlane at-once`); not yet clicked |
| 5 | T-0004 ended its run with "Waiting for clippy and fmt to finish." Claude Code had moved the long command to the background; a headless run exits when the turn ends, so the command died and nothing came back. The task was left `sleeping` with no follow-up, decision or event, so nothing would ever wake it, and the board said "waiting for a timer or an event". | fixed in 0cf8a72d (background tasks off for headless runs; one more turn, then the task is marked and the lead told) |
| 6 | The lead did not report after T-0003 finished: no message arrived in the 30 minutes after. It was probably waiting to report both tasks together, but T-0004 never finished, so nothing was said at all. | fixed in 32910401 (lead instructions) and 0cf8a72d (the lead is told when a task ends unfinished); not yet seen on a real run |
| 7 | T-0003 (README: routines, `session_max_age_secs`, `[github] gh`, the GitHub source) finished in one run, 34 lines, accurate against the code. | merged into v2 |
| 8 | Waking T-0004 with `threadlane send --task` resumed its session, and the run was cut while a foreground `cargo test` was still running, two to three minutes in. The transcript shows Claude Code first answering a leftover background-task notification ("No response requested.") before the hub's prompt. Confirmed: the driver ended the run on the first result and killed the process 10 s later. | fixed in 0cf8a72d (a run ends on the last result) |
| 9 | T-0004's code is otherwise good: pagination follows `rel="next"` up to 10 pages, 195 hub tests pass, fmt is clean. Clippy fails on one dead function (`parse_feed`), which is exactly the check that never got to report. The task is asleep again with nothing to wake it. | done: woken on the fixed build, run 13 fixed clippy and finished; merged into v2 as 36bb95ec. The lead reported it at 15:58 |
| 10 | The app showed T-0004 honestly as "ended quietly" on its timeline, but nothing on Home or the board marks a task that needs attention. | fixed in b2c55262; not yet seen with a real stuck task |
| 11 | A hub created with the default ceiling (On request) cannot raise it afterwards, so no agent on the `dev` hub can be set above On request now that modes are real. | fixed in b2c55262 (`threadlane ceiling <mode> --space-wide` and a Settings control); the `dev` hub is still On request |

Result of run 2: both tasks' work is in v2. T-0003 took one run. T-0004 took four: two cut short by the two engine bugs above, one woken by hand before the fix, and one after it that finished.

Still open: finding 2 (a lead's final text is not delivered, only `send_message`).

## Run 3, 2026-11-17: smoke test after the driver change

Milestone 10 stage A changed the Claude driver to feed prompts through streaming input. One small message to the General lead on the rebuilt hub: answered in one run.

| # | Finding | State |
|---|---|---|
| 12 | The driver's streaming input works on the real `claude` (start of a lead session, a reply through `send_message`). Fold-in during a run and a stop were not exercised. | checked |
| 13 | Asked which projects exist, the General lead said it can see only General. It has no view of the space's other projects. | open; the planned space lead (`Plan.md`, "Before the prerelease") addresses it |

## UI quality pass, on screen (2026-11-17)

Each stage is built by Codex, which cannot see the app, and then looked at against the `dev` hub.

| # | Finding | State |
|---|---|---|
| 14 | Stage 1 as delivered: icons with no asset drew gaps, tabs lost their labels, "Pause project", "+ New task" and Home's space filter were gone, settings rows unreadable. None of it visible to tests. | fixed in debcf967 after one round; a test now fails when an icon has no asset |
| 15 | The project header shifts between tabs (the column narrows on Conversation and a "Lead mode" line pushes the tabs down); "+ New task" has a plus icon and a "+". | fixed in stages 4 and 5 |
| 16 | Stage 2: nothing regressed, the View menu items and the Settings menu item work, tooltips show shortcuts. Letter and digit keys could not be sent to the app in the background, and the hub has no pending decision, so J/K, 1 to 9, `?` and the new decision card are unseen. There is no menu item that opens the shortcuts dialog. | `?` confirmed by the owner; Help > Keyboard Shortcuts added in stages 4 and 5; J/K, number keys and a live decision card still unseen |
| 17 | Stage 3 on screen: markdown, id links, the quiet-run line and opening at the latest message all work on the real conversation. Sending, the held send and undo, and code blocks were not exercised. | to check by hand |
| 18 | Stages 4 and 5 as delivered lost the board's Done column (columns no longer fit and the last one fell off), the machine's online badge, and showed "1 machines". | fixed after one round; a test pins the board's columns |
| 19 | Task page on T-0004 shows real usage from the hub: 3 runs, 9 min, 17042 output tokens, $1.1052. Confirm dialogs, right-click menus and the machine page were not opened. | to check by hand |

What only a person at the keyboard can check, in one list: J and K and the number keys on a real decision; typing, sending and undoing a send; a failed send keeping its text; right-click menus; each confirm dialog; a notification and answering from it; a narrow window.

## Run 4, 2026-11-17: starter agents (T-0005, T-0006), built by Threadlane

Goal: `docs/agent-templates.md` as two dependent tasks for the Engineer. Both finished without help; the work is in v2 as 92240a27.

| # | Finding | State |
|---|---|---|
| 20 | The lead acknowledged the tasks it created, said the second waits for the first, and when it would report. The fix for findings 3 and 6 holds on the real `claude`. | checked |
| 21 | The dependency worked: T-0006 started after T-0005 finished, and its branch contains T-0005's commit. Each took one run. | checked |
| 22 | Neither run ended early or with work in the background; both ran the three checks in the foreground. The run-ending fix holds. | checked |
| 23 | Merging T-0006 into v2 conflicted in four files with archive and delete, which had landed meanwhile and added requests in the same places. Resolved by hand; 674 tests pass. Parallel work on the client interface will keep doing this. | open: nothing in the system helps with it |
| 24 | The lead's final report did not reach `threadlane feed` when I looked (it had been read by an earlier watcher); the owner had to tell me the tasks were done. | open: check whether the lead reported, in the app |
