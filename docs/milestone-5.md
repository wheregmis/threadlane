# Milestone 5 design: memory and context

Maps `Plan.md` §15 milestone 5 onto the crates. Read with `Plan.md` §8,
§4.4, §5.1, §9.4 and invariant 12 of §13. It is built in three stages, in
order; each stage leaves `cargo test --workspace`, clippy with
`-D warnings` and `cargo fmt --check` passing.

## 1. Decisions

1. **One machine still.** Rotation "when it would resume on a different
   machine" is milestone 6; the other two causes (native session missing,
   age limit) are built here.
2. **Scope is a function of the run, checked in the store's caller, once.**
   One function in the hub maps (project, lane kind, path as the agent wrote
   it) to a stored path or a refusal. Every memory tool goes through it. No
   tool takes a project argument, so another project cannot be named.
3. **Paths the agent sees are relative to two roots**: `space/…` and
   `project/…`. The hub stores them as `space/…` and
   `projects/<project id>/…`. A path with `..`, an absolute path, or any
   other root is refused.
4. **Indexes are files the hub writes**, in the same transaction as the
   write that changes them. Agents cannot write a path named `INDEX.md`.
5. **A protected write that needs asking is a decision of kind `approval`**,
   like publishing (milestone 3). The stored call is the memory write
   itself; the hub applies it when the answer is approve, at most once, and
   the session resumes with the outcome. No `proposal` kind yet (milestone
   9).
6. **"A run you triggered"** means every event in the run's batch has
   origin `user`. A routine, a task result, or anything external is not.
7. **The final progress turn is one more turn in the same session**, asked
   by the runner when a task run is about to end without the progress file
   having been written in that run. It is skipped when the run failed or
   was stopped.
8. **No new dependencies.**
9. **Not built:** dreaming (milestone 11), a client view of memory and
   rollback from a client (the store keeps every version; a command to
   revert waits for a need), repository notes preloading (they are on
   demand, so the tools cover them).

## 2. Stage A: the store and the tools

- **Store.** `MemoryStore` already has read, write, delete, list, history.
  Add rename (a delete and a write in one transaction, history kept on
  both paths) and make every write carry the run that made it. Budgets: a
  write to a preloaded file (`instructions.md`, any `INDEX.md` source line,
  a task progress file) over its budget fails with a message telling the
  agent to split or trim. Budgets are constants in core, not settings.
- **Indexes.** Each note starts with a title line and a one-line
  description; a write without them is refused with that message. The hub
  rebuilds `notes/INDEX.md` of that directory, and `tasks/INDEX.md` for a
  progress file, sorted by path, so the same files always give the same
  bytes.
- **Scope.** The function of decision 2, with tests for invariant 12: a run
  in project A reading, writing, listing, renaming into or out of project
  B's directory, or the space's `instructions.md` from a task lane, each
  refused.
- **Tools**, tier 1, in both lane kinds: `memory_view` (a file, or a
  directory listing), `memory_create`, `memory_edit` (replace one exact
  string, refused when it is absent or not unique), `memory_delete`,
  `memory_rename`. Each is a recorded tool call like any other, so a retry
  after a crash does not write twice.
- **Protected instructions** (decision 5 and 6). A write, edit, delete or
  rename touching `space/instructions.md` or `project/instructions.md` in a
  run you triggered is applied. In any other run it opens a decision and
  the run ends `deferred`, as publishing does; approve applies the stored
  write once, decline resumes the session with the note. A changed argument
  hash applies nothing (invariant 7).

Tests: store tests for versions, rename, budgets and index bytes; hub tests
for each tool and each scope refusal; an exit scenario for a protected
write asked from a task run, with a kill at each step boundary.

## 3. Stage B: what each run is given

- **Appended instructions** (§8.5 items 1 to 3), built by the hub in
  `begin_run` and passed to the backend as today's instructions are: agent
  instructions; space instructions; project instructions; the space notes
  index; the project notes index; for the lead, the task index. Empty parts
  are left out with no trace. The bytes depend only on memory and the
  agent, never on the run, the time, or map iteration order.
- **The prompt** (items 4 to 7), in that order: for a task agent starting a
  fresh session, its progress file; for the lead, recent outbound messages
  (already there); open decisions in this lane; the triggering events. A
  resumed session gets only open decisions and events.
- **Outside content is marked.** Each event is rendered with its source,
  time and trust; `external` content is delimited as data. Keep what
  milestone 1 already renders and add what is missing.

Tests: the appended instructions of two runs of one agent in one project
are byte-identical when memory did not change and differ when it did; a
project instruction reaches a later session in that project and not a run
in another project; a space instruction reaches both.

## 4. Stage C: progress files, rotation, the exit

- **Task progress.** A task's file is `project/tasks/<task id>.md`; its
  agent can write only its own. `finish_task` requires it to exist.
- **The final progress turn** (decision 7): the runner resumes the session
  once with a fixed prompt asking for the progress file, then ends the run
  as it would have. One extra turn at most.
- **Rotation** (§8.6). In session routing, a session whose native session
  the backend reports missing, or that is older than the age limit (a
  `[timing]` setting, default 24 hours), is closed and a fresh one started
  from the progress file. A backend error that means "no such session" is
  recognised in each driver and is not a failed attempt.
- **The exit scenario**, on the scripted backend, sentence by sentence
  from the milestone's exit paragraph, with invariant 12 asserted over
  every memory version written during it.

## 5. Not in this milestone

Dreaming, memory in the desktop app, rollback from a client, checkpoints,
a second machine.
