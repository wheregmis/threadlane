# Autonomy rework: ask about intent, not steps

Status: proposed. Changes `Plan.md` §2.3 (modes, matrix, standing rules)
and the gate in `crates/threadlane-hub/src/gate.rs`. Each stage below
lands on its own, leaves `cargo test --workspace`, clippy with
`-D warnings` and `cargo fmt --check` passing, and can ship before the
next one.

## 1. Why

Measured on the Personal hub, 2026-11-18 to 2026-11-21 (391 decisions):

| Answered by | Approved | Declined |
|---|---|---|
| You, approvals | 99 | 0 |
| You, proposals | 21 | 1 (postponed a release check) |
| Classifier review | 184 | 0 |
| Standing rule / built-in default | 52 | n/a |

Over four days, about 120 interruptions changed one outcome. What you were
asked most:

| Ask | Times | Cause |
|---|---|---|
| Engineer `merge` | 19 | Engineer is Assisted; tier 3 asks below Autonomous and no rule is ever offered |
| Lead `create_task` | 22 | Proposals have no "always" option; stricter-of-two-modes with Assisted assignees |
| `report_threadlane_limitation` | 20 | Dream runs always ask (`gateway.rs`, dream branch); about 2 h wait each |
| Backend/Fullstack `merge` | 14 | Rules are per agent, so each developer role needs its own rule; older builds missed matches |
| `label_issue` | 7 | Rules were per issue number before #653 |
| `open_pull_request` / `publish_branch` | ~20 | Taint steps Autonomous down and turns off the own-branch default |

42 of the 44 merge decisions belonged to tasks you created (`origin = user`).
For those, every step after "do this task" repeats a yes you had already
given.

The ceiling has no effect today: the space, both projects and 11 of 13
bindings are Autonomous, and a ceiling only caps.

## 2. Principles

1. **Approve intent once.** A task you requested authorizes the steps that
   carry it out on its own work: the task's own branch, or the branch of
   another requested task in the project, and their pull requests.
2. **Machine checks over clicks.** A merge is guarded by facts the hub can
   check: the PR is the task's own, the reviewer approved this head, CI is
   green, and there are no conflicts. A human answering after 12 minutes
   without reading is not a guard.
3. **Outside content limits reach, not routine.** Taint keeps blocking
   actions that leave the task's scope. It no longer turns the task's own
   draft PR into a question.
4. **One knob per project.** A project has one mode. Agents follow it
   unless set to Off or deliberately overridden.
5. **Interrupt only for what is irreversible and outside the task.** Send
   the rest to a list you can veto later.

Not doing: routing decisions to the lead first. It makes per-agent trust
meaningless, and the lead is a weaker reviewer than the no-tools
classifier: a full agent, more context, more exposure to prompt injection.

## 3. Stages

### Stage A: a requested task's own work (largest win)

**Gate input.** Add to `GateInput` (`gate.rs`):

```rust
/// The run's task is work you requested (TaskOrigin::is_requested), and the
/// call acts on own work, checked by the hub: the task's own branch, or the
/// pull request of its own or another requested task's branch in the project.
pub own_work: Option<OwnWork>,

pub struct OwnWork {
    /// For merge: the reviewer approved the PR's current head, CI is green,
    /// and it merges cleanly. Always true for the other own-work tools.
    pub checks_pass: bool,
}
```

**Own work** is the task's own branch, or the branch of another requested
task in the project. A separate task you requested, such as "Merge PR #557
after recheck", may ready, edit and merge another requested task's pull
request; a task an agent started, or one in another project, may not.

**New row.** `Row::OwnWork` applies to `publish_branch` (no `onto_task`),
`open_pull_request`, `mark_ready`, `edit_pull_request`, and `merge`, when
`own_work` is `Some`. Its cells:

| | On request | Assisted | Autonomous |
|---|---|---|---|
| untainted, checks pass | Allow | Allow | Allow |
| tainted, checks pass | Review | Review | Review |
| checks fail (merge only) | Ask | Ask | Ask |

On request also allows, because you asked for exactly this work. That is
the definition of On request ("starts work only when you ask").
Overrides still make the row stricter. Forbidden and Off still block
first.

**Deciding `own_work`** (`gateway.rs::gate`, before calling the gate):

- `lane.task_id` is `Some` and the task's `origin.is_requested()`
  (already used by `timers::run_requested`).
- For `publish_branch`/`open_pull_request`: the branch is
  `task_branch(task)` (`threadlane-core/src/workspace.rs:214`).
- For `mark_ready`/`edit_pull_request`/`merge`: the PR's head, in the same
  repository (not a fork), is `task_branch(t)` for a task `t` of the same
  project that works in the repository and is itself requested; `t` may be
  the calling task. `branch_task` (next to `task_branch`) reads `t` from the
  head ref. Add to `GithubSource`
  (`threadlane-core/src/github.rs`):

  ```rust
  /// Head ref, head commit, draft flag, mergeability and the combined
  /// status of required checks for pull request `number`.
  async fn pull_request_state(&self, repository: &str, number: u64)
      -> Result<PullRequestState, GithubError>;
  ```

  with a default that returns `Unsupported`, the same way `issue_claim`
  does. The `gh` implementation uses
  `gh pr view N --json headRefName,headRefOid,isDraft,mergeStateStatus,statusCheckRollup`.
  Read it the way `publish::issue_claim` is read: once, after the call is
  recorded, so a replay doesn't read GitHub again.
- `checks_pass` for `merge`: `mergeStateStatus == CLEAN`, every required
  check succeeded, and `t`'s latest `task_reviews` row has verdict
  approved with a revision for the repository equal to `headRefOid`.
- Any read error means `own_work = None`, so the call falls back to
  today's matrix (fail closed).

**Recorded as.** `Verdict::AllowByDefault` exists already, so reuse it:
the decision is stored answered `by_default`. The card in Activity says
"allowed: own task, checks passed".

**Tests.** Table rows in `gate.rs` for each cell above, plus overrides,
Off and Forbidden. A hub test in `crates/threadlane-hub/tests/hub/permissions.rs`:
a user task merges its own PR with checks green and no decision is shown.
The same PR with a failing check asks you. A PR whose head is another
branch asks you. A requested merge task merging a sibling requested task's
approved, green PR is allowed with no decision shown; the same with a sibling
an agent started, or in another project, asks you.

**Plan.md.** Add the row to the §2.3 matrix. Replace "Default for a
task's own work" with the own-work rule.

**Expected effect on the sample above:** about 60 of the ~120 asks go away
(merge, publish, open PR and mark ready on user tasks).

### Stage B: standing rules cover the project, not one agent

- `gateway.rs::gate`: drop `.filter(|rule| rule.agent_id == agent)`. A rule
  matches any agent bound in the project. Keep the `agent_id` column as
  "created for" (audit only).
- `allow_by_rule` already counts `uses`. Also count a use when a tier-3
  rule turns Ask into Review (`reviewed_decision` path), so a merge rule
  stops showing `uses 0`.
- Desktop rules list: show "created from \<agent\>" instead of scoping the
  rule to that agent.
- Plan.md §2.3 "Standing rules": "scoped to one project, one tool, and one
  argument pattern".
- Tests: a rule created from Backend's decision allows Fullstack's
  matching call. A revoked rule allows nobody.

### Stage C: one mode per project

**Model.** `bindings.mode` becomes nullable. `NULL` means "follows the
project". Effective mode = `binding.mode.unwrap_or(project ceiling)`,
still capped by the ceiling. `Off` stays explicit.

**Migration.** Set `mode = NULL` where it equals the project's effective
ceiling. Bindings below it (Engineer, Assisted) keep their value. They show
as overridden, so you see exactly the one exception.

**Desktop (`screens/project.rs::agents_tab`).**

- The top card is renamed **Project mode** (it is the ceiling) and keeps
  the four-way control.
- Each agent row replaces the four-way control with **On / Off** and
  "Follows project". An overflow menu offers "Set its own mode…" for the
  rare exception, which then shows inline.
- The header subtitle shows one value (`Mode · Autonomous`) instead of
  `Ceiling · … Lead mode · …`.

**Core.** `Mode` doesn't change. `SetMode` (`threadlane-core/src/client.rs`)
takes `mode: Option<Mode>`. The `create_task` stricter-of-two rule reads the
assignee's effective mode, so it follows the project too.

**Tests.** The store migration test, the gate with an inherited mode, the
desktop snapshot for the agents tab, and the client CLI round trip.

**As built.** The agent row's On / Off is a switch, and "Set own mode…" is a
small button beside "Follows project" rather than an overflow menu. New
bindings follow the project: the lead a project gets at start, an agent added
from the Team tab, and `threadlane mode <agent> follow` (also `project`). The
migration keeps a mode above the ceiling as it is; it still acts at the
ceiling.

### Stage D: taint limits reach, not routine

- In `gate.rs`, the built-in default for `publish_branch` (own branch)
  and `open_pull_request` (draft) gives **Review** instead of being
  dropped when the run is tainted. Today a tainted Autonomous run asks
  you about its own draft PR.
- Unchanged: tainted runs still step down for every other tier-2/3 call
  (comments, issues, other repositories, other PRs, messages, protected
  memory). Screening (`permission_questions` with `verdict = clean`)
  still clears taint as it does today.
- Tests: a tainted own-branch publish gives a Review verdict, and a
  tainted comment on another PR keeps today's verdict.

### Stage E: send low-stakes asks to a list

- **Limitation reports from dreams** go through classifier review
  (`Question::LimitationReview` already exists) instead of always asking.
  `require_approval: true` still asks.
- **Proposals** (initiative row) get a fourth option, "always start this
  kind here". It creates a standing rule on `create_task` whose pattern is
  the assignee, for example `{"agent": "Reviewer"}`. Add `agent` to
  `TARGET_FIELDS` for `create_task` only.
- **Urgency.** `NewDecision` gets `urgent: bool`, false for proposals and
  limitation reports. The desktop doesn't notify for non-urgent
  decisions; they collect in an Inbox "Later" group. Expiry is unchanged.

### Stage F: keep measuring

Add `threadlane decisions stats [--days N]` to the client CLI. Per
(project, tool, who answered) it shows counts, decline rate and median
wait, using the query this doc's §1 table came from. It's the acceptance
check for every stage, and the signal for the next one: any (tool,
pattern) you approved 10 times in a row without declining is offered as a
project rule on the next card.

## 4. Order and acceptance

1. **A** (own work), then **B** (project rules). Together they should
   take the sample from ~120 asks to ~35.
2. **C** (one mode). It mostly removes the Engineer and proposal
   exceptions, and makes the agents screen one control.
3. **D**, **E**, **F**.

Target after A–E, on the same kind of week: **under 15 decisions reach
you**, and every class that does has a decline rate above zero or is
irreversible outside a task (force-push, deletion, merging a PR that is
not the task's own, messaging other people, spending money).

## 5. Risks

- **A merge without a human.** This is the deliberate trade. It's guarded
  by: requested task only, own branch only, reviewer approval at the
  exact head, green CI, a clean merge, and Review (not Allow) when the run
  read outside content. Repositories without CI or a reviewer binding fail
  `checks_pass` and ask as today.
- **GitHub read failures** fall back to today's matrix. They never allow.
- **Project-wide rules** widen what one click grants. The rules list and
  "created from" keep that visible, and revoking a rule takes effect on
  the next call.
- **The migration** in C only clears modes equal to the ceiling, so no
  agent gains authority it didn't already have.
