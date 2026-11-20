# GitHub presence: show work in progress where the team looks

## Problem

Threadlane appears on GitHub only when work is finished: a branch, a pull
request, a comment. While a task runs, nothing on GitHub says so. A teammate
can pick up the same issue, and nobody outside this hub sees progress.

GitHub is already the team's shared board, so the fix is to use it, not to
sync hubs. Three small changes, shipped as three stacked pull requests.

## What is true today

- The hub reads GitHub with `GET` only (`crates/threadlane-hub/src/gh.rs`).
- Shared effects are forge tools a task agent calls. The gate asks you, and the
  runner performs the stored call (`crates/threadlane-hub/src/publish.rs`,
  `crates/threadlane-runner/src/forge.rs`).
- A task has no link to an issue. An issue reaches a task only as words in its
  goal.
- `open_pull_request` already opens a draft. Agents are told to call it at the
  end of the work (`DRAFT_PR_GOAL`, `TASK_RULES`).

## Decisions

1. **Agent-driven, like every other forge effect.** No issue field on a task
   and no schema change. The rules tell the agent when to call the tools.
2. **Claiming follows the existing approval rules.** `assign_issue` is tier 2
   (shared, reversible), the same as `comment`. You are asked the first time
   and can make a standing rule per repository.
3. **A held issue always asks you.** A standing rule never takes an issue
   that someone else holds.
4. **The hub still never writes to GitHub.** The check is a `GET` in the hub.
   The assignment is performed by the runner.

## PR 1: claim on start (`assign_issue`)

A new forge tool for task lanes.

- Arguments: `repository` (registered in the project), `number`.
- `RepositoryAction::AssignIssue { number }`.
- The runner performs `gh issue edit <number> --add-assignee @me` against the
  registered remote, built the same way as the other `apply` actions.
- Decision question: `Assign issue #<n> in <repository> to you?`
- It is added everywhere the `comment` tool is listed: definitions, tiers, the
  gate's rule pattern, the perform path, and the desktop's permission and
  decision labels ("assign issues to you").
- `TASK_RULES`: when the task works on a GitHub issue of one of its
  repositories, call `assign_issue` for it before any other work.
- `LEAD_RULES`: when a task works on a GitHub issue, name the repository and
  the issue number in its goal.

## PR 2: check before start

Before the decision on `assign_issue` is made, the hub reads the issue.

- `GithubSource` gains one read: the issue's state, its assignees, the open
  pull requests that reference it, and the login `gh` is using. All `GET`:
  `repos/<o>/<n>/issues/<number>`,
  `repos/<o>/<n>/issues/<number>/timeline?per_page=100` (events of type
  `cross-referenced` whose source is an open pull request), and `user`.
- Outcomes:
  - **Closed issue:** the call is refused with that reason.
  - **Assigned only to you:** the call answers "already assigned to you", with
    no decision and no write.
  - **Free:** the normal gate applies (ask, or a standing rule).
  - **Held:** assigned to someone else, or an open pull request by someone
    else references it. A decision is always opened, no standing rule applies
    and none is offered. The question names who holds it:
    `Issue #7 in app is assigned to alice and has open pull request #12. Take it anyway?`
    Declining tells the agent to stop and report through `finish_task`.
  - **GitHub cannot be read:** treated as held, with the reason in the
    question. Never assume free.
- `TASK_RULES`: when `assign_issue` is declined, do no work on the issue.

## PR 3: draft pull request early

No new tool. The draft pull request becomes the public progress record.

- `TASK_RULES` and `DRAFT_PR_GOAL`: open the draft pull request as soon as the
  first working change exists, not at the end. Push later changes to it with
  `publish_branch`. When the task fixes an issue, the body starts with
  `Closes #<n>`, which links the pull request to the issue, so the PR 2 check
  of any other hub sees it.
- The pull request stays a draft. Marking ready stays your call.

## Not in this stage

- A progress comment on the issue. The draft pull request says most of it.
- An issue field on tasks, or tasks created from issues automatically.
- Releasing the claim (`--remove-assignee`) when a task is cancelled.
- Hub-to-hub sync.

## Checks for every PR

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`
and `cargo fmt --check`, run through `rtk proxy mbx`.
