# Milestone 11 design: dreaming, evals, operations

Maps `Plan.md` §15 milestone 11 onto the crates, without guest machines,
which wait for the second machine. Read with `Plan.md` §8.4, §14 and §16.
It is built in two stages; each leaves `cargo test --workspace`, clippy
with `-D warnings` and `cargo fmt --check` passing.

## 1. Decisions

1. **Metrics are computed from the stores on request**, not collected into
   a metrics system. The hub already records every event, run, timer,
   decision and outbox message with its times; one read-only request
   returns the numbers of §14.1 that make sense on one machine. No new
   dependency, no exporter, no background collection.
2. **Tracing is one id.** An event gets a trace id when it is inserted; a
   run takes the id of its oldest event; tool calls, decisions and outbox
   messages take their run's. "Inspect a trace" is a read over the stores.
3. **Operator tools are client requests with CLI commands**, not a
   separate tool: inspect a lane, replay a dead event, roll back a memory
   file to a version. Pause a project exists. Drain and revoke a machine
   wait for the second machine.
4. **Dreaming is an ordinary lead run** (§8.4) started by a system timer
   per project, with a restricted tool set: read that project's sessions
   since the last dream, inspect all its tasks, and curate its memory. A dream can
   report a Threadlane limitation to `wheregmis/threadlane` through the shared-effect
   permission gate. Each issue describes a public-safe situation and suggested improvement.
   Previous report titles and outcomes help prevent repeats. A change to an
   instructions file is a proposal, never applied. On by default per
   project, daily at 02:00 in the hub's zone; a setting changes the cadence
   or turns it off, and an explicit off stays off.
5. **"Measurably reduces stale and duplicate memory"** is measured by a
   deterministic checker over memory (exact and near-duplicate notes by
   normalized text, notes that point at tasks or files that no longer
   exist) run before and after a dream on a seeded corpus. In `cargo
   test` the dream is scripted, which proves the mechanics and the
   checker; the measured effect of a real model is a live eval.
6. **Evals are live and ignored by default**, like the red-team suite:
   each of §14.2's evals that applies on one machine is a small set of
   scenarios with a scorer, run against the real backend on demand and
   printing a score. The regression eval replays recorded event streams
   from a directory. `cargo test` runs every scorer against scripted
   transcripts so the scorers themselves are tested.
7. **The load test is a deterministic simulation**: many projects and
   lanes, messages, timers, decisions and answers arriving at random from
   a seed, with random process kills at step boundaries, then every
   invariant asserted. A small size runs in `cargo test`; a large size is
   an ignored test. Machine kills wait for the second machine.
8. **No new dependencies.**

## 2. Stage A: tracing, metrics, operator tools, the load test

- **Trace id** of decision 2: a column where one is missing, set at
  insert, carried through; shown on the run record.
- **Metrics** of decision 1 as one `operations` capability request:
  lane lag, queue depth per project, run durations, account cooldowns and
  time spent waiting on them, dead events, timer lateness, outbox lag,
  messages per day, silent-run ratio, decision rate and answer latency,
  standing rules created. `threadlane metrics [-p project]`.
- **Operator requests** of decision 3: `threadlane lane <project|task>`
  (its events with status and attempts, its sessions and runs, its lease,
  open decisions, timers), `threadlane replay <event>` (a dead event
  becomes pending again with attempts reset, once), `threadlane memory
  rollback <path> <version> [-p project]` (writes that version's content
  as a new version by you), `threadlane trace <id>`.
- **The load test** of decision 7, with the property-test shape of §16:
  the seed is printed on failure so a run reproduces.

Tests: trace ids connect an event to its run, tool calls, decision and
outbox message; each metric against a hand-built history on the fake
clock; replay revives a dead event exactly once and it then runs; rollback
restores content, keeps history and obeys budgets; the load test at small
size with a fixed set of seeds.

## 3. Stage B: dreaming and evals

- **Dreaming** of decision 4: the per-project setting and cadence, the
  system timer, the restricted tool set enforced at the hub (a call to
  any other tool is refused, not only hidden), a tool to read the
  project's sessions since the last dream (run outcomes, rationales and
  sent messages, not full transcripts), and the last-dream marker.
- **The memory checker** of decision 5, also offered as `threadlane
  memory check [-p project]`.
- **Evals** of decision 6 under one crate or test target, with
  `docs/evals.md` on how to run them and what each score means.
- **The app**: a Dreaming switch and cadence on the project's Memory tab,
  the last dream's time and what it changed (from memory versions written
  by a dream run), and a Metrics card on the Settings screen per space.

Tests: a dream run is offered and can call only its tools; it never
crosses a project (invariant 12); an instructions change becomes a
proposal; a scripted dream on a seeded corpus lowers the checker's
counts; a dream never runs when the setting is off, and coalesces after
downtime; each eval's scorer on scripted transcripts.

## 4. Not in this milestone

Guest machines, machine metrics and drain or revoke from operator tools,
a metrics exporter, running the live evals.
