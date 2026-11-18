# Milestone 3 design: tasks on the first machine

Maps `Plan.md` §15 milestone 3 onto the crates. Read with `Plan.md` §4.6,
§5.1 to §5.5, §5.7, §9.2, §9.4 and §16. It is built in four stages, in
order; each stage leaves `cargo test --workspace`, clippy with
`-D warnings` and `cargo fmt --check` passing.

## 1. Decisions

1. **Everything stays on one machine.** Checkpoint bundles, cross-machine
   restore, placement affinity and patience are milestone 6. A run ends with
   a commit on the task branch and nothing is uploaded.
2. **Until milestone 9 every binding acts as On request**, and Off never
   runs (already enforced). A task inherits `origin` from what created it; a
   task whose origin is `user` or `user_routine` is requested work, and so
   are its subtasks and its `task_result`.
3. **The gate has one new outcome.** Tier 3 tools ask. Asking opens a
   decision of kind `approval`; `input` (`ask_user`) and `proposal` wait for
   milestone 9.
4. **Shared effects go through one interface**, `Forge`, with two
   operations: push a branch, open a draft pull request. The real one runs
   `git push` and `gh pr create --draft` in the task's worktree. Tests use a
   fake that records calls. Nothing in `cargo test` touches a network or a
   real remote; test repositories are local bare repositories in a temp
   directory.
5. **A stored call is performed by the runner, not the agent** (§9.4). The
   tool returns "waiting on you, end your turn", the run ends `deferred`,
   and the run that follows the answer performs the stored call first, then
   resumes the session with the outcome. The stored call is a tool call
   like any other, so it takes effect at most once (invariant 5).
6. **A second backend, same interface.** `threadlane-backend-codex`
   mirrors `threadlane-backend-claude`: same `AgentBackend`, same test
   shape (a fake `codex` script for `cargo test`, an ignored live test).
7. **No new dependencies** unless a stage cannot be done without one; say
   so in the report if one is added.

## 2. Stage A: tasks

Hub and runner, scripted backend only. No git, no decisions.

- **Store.** `TaskStore` grows to the lifecycle of §5.2: create with its
  task lane in one transaction, idempotent on `created_by_call`; status
  changes that refuse an illegal transition; `depends_on`;
  `waiting_reason`; `result`; cancel. Short ids of the form `T-0142`.
- **Events.** Creating a task puts a `task_assigned` event in its lane.
  Finishing, failing or cancelling one puts a `task_result` event in the
  project's lead lane, once.
- **Tools.** Lead lanes get `create_task`, `update_task`, `cancel_task`,
  `list_tasks` next to `send_message`. Task lanes get `finish_task` and
  `create_task` (subtasks, `parent_task_id` set, origin inherited). A lane
  is offered only its own kind's tools, and a call to another kind's tool
  is refused at the hub, not only hidden.
- **Eligibility and placement** (§5.2, §5.4 items 1 and 2). A task lane is
  granted only when its dependencies are `done`, its project is not paused,
  and the project's `max_parallel_tasks` and the binding's `max_parallel`
  have room. A queued task that is not running carries the reason.
- **Run cycle for a task lane.** `begin_run` builds the prompt from the
  task's title and goal; the task moves `queued` to `running`, to `done` on
  `finish_task`, back to `queued` on a failed run, to `failed` when
  attempts run out, and to `sleeping` when a run ends without finishing.
  The runner's "task lanes are not supported" failure is removed.
- **Backend tool removal.** A lead run is started with the backend's own
  file and shell tools removed (§5.1).

Tests: store tests for every transition and for idempotent creation; hub
tests for each tool, eligibility and limits; exit scenarios "one goal
becomes three tasks, two run at the same time, the lead sends one combined
result" and "a dependent task waits"; the crash scenarios extended to the
task-lane steps.

## 3. Stage B: workspaces, deny rules, accounts

- **Repositories on the client interface.** `OrgRequest::AddRepository`
  and `Repositories`, with `threadlane repo add <name> <remote> [-p]` and
  `threadlane repos`. A remote already registered in this space is refused.
- **Workspaces** (§5.5). A new `Workspaces` interface in core, implemented
  in the runner with `git`: under the runner's data directory, one clone
  per repository and one worktree per task per repository on the branch
  `threadlane/<task id>`. Setting up is idempotent; a run on an existing
  worktree reuses it. When a run ends, uncommitted work is committed on the
  task branch. A task run's working directory is its worktree (the first
  repository's when it names several). Invariant 15 is asserted in the exit
  tests: no two tasks resolve to the same path.
- **Deny rules** (§9.2 layer 2). One list in core of the direct forms that
  are refused: `git push`, `gh pr create`, `gh pr merge`, `gh release`,
  `gh repo delete`, and force variants. Every task run is started with full
  access and this list; each backend driver translates it. Appended
  instructions tell the agent to use the system's tools (layer 3).
- **Accounts** (§5.3). A backend event for "usage limit reached, resets at"
  already exists or is added; the runner reports it over a new `account`
  link call; the hub sets the cooldown, returns the run to the queue
  without using an attempt, and placement skips a lane none of whose
  agent's backends has a free account, marking its task `blocked`. When
  nothing in the space can run the hub sends one system notice, and one
  more when work resumes. Work resumes by itself when the cooldown passes.

Tests: workspace tests against local bare repositories; a driver test per
deny rule against the fake `claude`; exit scenario "no account: tasks show
blocked, none fails or uses an attempt, one notice, work resumes when the
limit resets".

## 4. Stage C: decisions, publishing

- **Gate.** `Verdict::Ask` opens a decision: tool, arguments,
  `arguments_hash`, question, options (approve, decline), recommended
  option. A decision card goes to the outbox. The tool call returns
  "waiting on you, end your turn" and the run ends `deferred`; the runner
  stops a backend that keeps going. The task is `waiting_on_you`.
- **Answering.** `HubApi` gains listing pending decisions and answering
  one; `threadlane decisions` and `threadlane answer <id> <option>
  [--note]`. An answer is stored once (a second is ignored) and arrives in
  the lane as an event.
- **Performing.** The run after the answer loads the decision; if approved
  and the stored arguments still hash to `arguments_hash`, the runner
  performs the call through `Forge`, records the outcome, and resumes the
  session with it. Declined: the session resumes with the note. In a task
  lane a message from you while a decision is open declines it with the
  message as the note.
- **Tools.** `publish_branch` and `open_pull_request` (always a draft,
  publishes its branch in the same call), tier 3, task lanes only. Both
  take a repository by its registered name and refuse one not registered in
  the run's project (invariant 19, as far as these two tools go).
- **Result.** `finish_task` records the branch and pull request in the
  task's `result`.

Tests: gate and decision tests at the hub; exit scenario "a finished task
asks before publishing; after the answer its branch is pushed and a draft
pull request opened, exactly once", including a kill at every new step
boundary; a decline scenario; invariant 7 (a changed argument hash performs
nothing) and invariant 8 (no table or log line holds a token; the system
only invokes `git` and `gh`).

## 5. Stage D: Codex, conformance, the whole scenario

- **`threadlane-backend-codex`.** `codex exec --json`, resume by id,
  the tool server through its configuration, `--sandbox
  danger-full-access`, deny rules as execution policy rules, the usage
  limit recognised from its stream, usage reported. `[backends.codex]` in
  `hub.toml`, like `[backends.claude]`.
- **Backend choice.** A run uses the first of its agent's backends that is
  installed, logged in, and has an account that is free; none means
  blocked. A task that changes backend starts a fresh session.
- **Conformance** (§16). One suite, written once against `AgentBackend`
  and run for both drivers: unattended run, event stream, resume by id, the
  system's tools reachable, each deny rule refusing its command, usage
  limit recognised. It runs in `cargo test` against the fake tools and,
  ignored by default, against the real ones.
- **The milestone's exit scenario**, end to end on the scripted backend
  and the fake forge: one goal, several tasks in parallel in separate
  worktrees of one repository, a dependent task waiting, one combined
  result, ask before publishing, pushed and opened exactly once, blocked
  and resumed on a usage limit. Invariants 7, 8, 15.

## 6. Not in this milestone

Checkpoint bundles and restore, a second machine, placement affinity,
`ask_user`, proposals, standing rules, decision expiry, the mode matrix,
`comment` and `merge`, memory and progress files, the desktop app.

The exit line "passes with the Mac as primary and with the Linux machine
as primary" needs a Linux machine and is checked there, not in `cargo
test`.
