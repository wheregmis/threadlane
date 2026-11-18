# Milestone 8 design: timers, routines, and GitHub

Maps `Plan.md` §15 milestone 8 onto the crates. Read with `Plan.md` §6.3,
§6.6, §7, §2.3 (requested work) and invariants 4 and 19 of §13. Milestones
6 and 7 (a second machine, resilience) are skipped for now, so everything
here runs on the hub's own machine. It is built in two stages, in order;
each leaves `cargo test --workspace`, clippy with `-D warnings` and
`cargo fmt --check` passing.

## 1. Decisions

1. **A recurrence is a small rule of our own, not RRULE.** One of: every N
   minutes or hours; daily at a local time; on named weekdays at a local
   time; monthly on a day at a local time. With a named time zone. Stored
   as JSON in `timers.recurrence`. It covers "9am daily", "weekdays at
   8:30", "every 2 hours", which is what the plan's examples need.
2. **One new dependency is allowed, for time zones.** Daylight-saving
   rules cannot be written by hand. Use one maintained crate with the time
   zone database built in, pinned, and nothing else from it leaks into
   core's public types: core stores the zone as a string and the rule as
   plain fields; the hub computes the next occurrence.
3. **`requested` comes from the creator** (§7.3): a routine you create is
   requested work; a timer an agent creates fires an event that is
   requested only if the run that created it was.
4. **An agent creating a schedule asks first**, as a decision of kind
   `approval`, like publishing. A follow-up does not ask. (Milestone 9
   lifts the ask in Autonomous mode.)
5. **Guardrails are constants in core**: at most 20 active timers per
   project, a schedule no more often than every 15 minutes, a follow-up no
   sooner than 1 minute. A refusal says which limit and its value.
6. **GitHub is reached only through `gh`**, behind one interface,
   `GithubSource`, with a real implementation that runs `gh api` and a fake
   for tests. Nothing in `cargo test` runs `gh`. The hub never marks a
   notification read and never writes to GitHub from the source.
7. **Until milestone 9 the initiative rule is the simple one already in
   force**: an event that is not requested work is recorded and starts no
   run. So a review request that belongs to no task is stored in the lead
   lane and waits; an event about a task's own branch or pull request
   continues that task and does start a run.
8. **Pulled events carry `external` trust** and are rendered as data
   (already done in milestone 1's prompt rendering).
9. **Not built:** decision expiry and the dreaming cadence on timers
   (milestones 9 and 11), job-result sources (milestone 10), `gh webhook
   forward`.

## 2. Stage A: timers and routines

- **Store.** `TimerStore` already creates, lists due, fires and cancels.
  Add: list by project, pause and resume, and the count the cap needs. The
  fired event's source id is derived from timer id and due time (already
  the contract of `fire`).
- **Recurrence.** The rule of decision 1 in core; "next occurrence after
  T" in the hub, in the rule's zone. A local time that does not exist on a
  spring-forward day fires at the first valid time after it; one that
  occurs twice on a fall-back day fires once.
- **Scheduler.** A task in the hub's process, every `[timing]
  scheduler_secs` (the setting exists; default 10): due timers, each fired
  in one transaction, then a wake hint. Missed occurrences coalesce: one
  fire, then the next occurrence after now.
- **Tools.** `set_follow_up` (a delay or a time, an instruction; goes to
  the calling lane), `create_schedule` (asks first), `list_timers`,
  `cancel_timer`. Lead and task lanes both get them; a task's timers go to
  its own lane. Creation is idempotent on the tool call.
- **Run cycle.** A `timer_fired` event is rendered in the prompt with the
  timer's instruction, who created it and when it was due.
- **Client interface.** A `timers` capability: list, create a routine
  (yours, so requested), pause, resume, cancel. `threadlane routines`,
  `threadlane routine add <instruction> --every … | --daily HH:MM |
  --weekdays … --at HH:MM [--zone …] [-p project]`, `routine pause`,
  `resume`, `cancel`. The app's Routines tab is a UI stage, not this one.

Tests: next-occurrence tests across a spring-forward and a fall-back
change in two zones; store and scheduler tests on the fake clock (fires
once, a kill between the transaction's steps fires nothing twice,
downtime across several occurrences fires once); each tool with each
guardrail; `requested` from the creator; an exit scenario "an agent sets a
follow-up and wakes on time" and "a routine you created fires at the right
local time across a daylight-saving change".

## 3. Stage B: the GitHub source

- **`GithubSource`** in core: the notifications feed since a cursor (with
  the poll interval the service asks for and "not modified"), and for a
  pull request: check results, reviews and comments since a cursor.
  `GhSource` in the hub runs `gh api`; `FakeGithub` in the testkit.
- **Poller.** One per hub, started only when the hub's machine reports
  `gh` logged in. Each pass: the feed, then each active task that has a
  pull request or a pushed branch (from the tasks' results and decisions'
  outcomes). Cursors in `source_cursors`. An unchanged answer costs
  nothing and inserts nothing.
- **Normalizing and routing** (§6.3, §6.6). Each item becomes an event
  with source `github`, the service's own id as `source_event_id`, trust
  `external`. An item about a task's branch or pull request goes to that
  task's lane and continues it; anything else about a registered
  repository goes to that project's lead lane, not requested; an item
  about a repository not registered in this hub is dropped and creates no
  event (invariant 19).
- **A waiting task wakes.** A task whose run ended without finishing is
  `sleeping`; an event in its lane makes it eligible again. A check result
  on its pull request therefore wakes it on the next pass.

Tests: normalizing each kind of item; routing to a task lane, to a lead
lane, and dropping; the same item twice inserts once; after the fake clock
jumps an hour with the poller stopped, everything from that hour arrives
exactly once; the source never issues a write (the fake records every
call and the test asserts none marks read); invariant 19; an exit scenario
"a check result on a task's pull request wakes that task within about a
minute" and "a review request that belongs to no task is recorded and not
acted on".

## 4. Not in this milestone

The Routines screen in the app, decision expiry, dreaming's cadence, more
sources, anything on a second machine.
