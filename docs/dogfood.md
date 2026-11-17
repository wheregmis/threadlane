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
| 3 | The lead created both tasks but sent no message saying so; `threadlane send` waited 300 s and gave up. In run 1 it did reply. | fix in progress (milestone 9 stage B, lead instructions) |
| 4 | The two tasks were asked to run at the same time; one ran. The Engineer's binding allows one run at a time by default, and nothing in the app or CLI says why the other waits. | fix in progress (UI stage 6: the reason on the card, and an editable "at once" per agent) |
| 5 | T-0004 ended its run with "Waiting for clippy and fmt to finish." Claude Code had moved the long command to the background; a headless run exits when the turn ends, so the command died and nothing came back. The task was left `sleeping` with no follow-up, decision or event, so nothing would ever wake it, and the board said "waiting for a timer or an event". | fix in progress on branch `fix-quiet-end`; the task was woken by hand with `threadlane send --task T-0004` |
| 6 | The lead did not report after T-0003 finished: no message arrived in the 30 minutes after. It was probably waiting to report both tasks together, but T-0004 never finished, so nothing was said at all. | fix in progress (milestone 9 stage B: the lead acknowledges new tasks, and speaks at once when a task ends unfinished) |
| 7 | T-0003 (README: routines, `session_max_age_secs`, `[github] gh`, the GitHub source) finished in one run, 34 lines, accurate against the code. | merged into v2 |
| 8 | Waking T-0004 with `threadlane send --task` resumed its session, and the run was cut while a foreground `cargo test` was still running, two to three minutes in. The transcript shows Claude Code first answering a leftover background-task notification ("No response requested.") before the hub's prompt. Likely the driver takes that first result as the end of the run and stops the process. | fix in progress on `fix-quiet-end` |
| 9 | T-0004's code is otherwise good: pagination follows `rel="next"` up to 10 pages, 195 hub tests pass, fmt is clean. Clippy fails on one dead function (`parse_feed`), which is exactly the check that never got to report. The task is asleep again with nothing to wake it. | waiting for the fix, then wake it again as the test of the fix |
| 10 | The app showed T-0004 honestly as "ended quietly" on its timeline, but nothing on Home or the board marks a task that needs attention. | fix in progress (UI stage 6: "Needs attention" on Home) |
| 11 | A hub created with the default ceiling (On request) cannot raise it afterwards, so no agent on the `dev` hub can be set above On request now that modes are real. | fix in progress (UI stage 6: a request and a Settings control for the space ceiling) |
