# Executing an agreed plan

Use for direct implementation or an assigned worker task after the design is settled. Follow project instructions and the task's ownership and authorization limits.

## Before editing

Read the task brief and relevant current files. Confirm the scope, design, dependencies, acceptance criteria, and check commands are sufficient. If a consequential detail is missing, report NEEDS_CONTEXT with the exact question before editing; do not independently redesign the task. Revalidate changed assumptions on a follow-up. Read-only research needs a scoped question, not an implementation plan.

## Implement and verify

- Follow the ordered steps; reuse existing helpers and keep the diff within the assigned files/scope.
- For changed behavior, write the smallest useful regression test. Run it before the fix and confirm it fails for the intended reason; implement the minimal fix, rerun the focused test, then refactor if needed while green. If the environment or nature of the change prevents a meaningful red/green test, disclose that limitation rather than invent evidence.
- Run focused checks first. Repair only relevant failures and rerun the failing checks while iterating. Run any agreed broad gate once after final edits, not on every change. Keep a record of commands, exit status, revision, and outcomes.
- Preserve user edits and healthy running processes. If a dependency, API, or environment contradicts the plan, pause and report the smallest concrete blocker and partial work.
- A worker must not spawn implementers or reviewers. Self-review is reading your own diff; final acceptance and any independent review belong to the parent.
- Commit or publish only within the assigned authorization boundary. Do not merge, push, or clean up worktrees merely because a generic workflow suggests it.

## Report

Self-review against the acceptance criteria and remove unintended changes. Return DONE, DONE_WITH_CONCERNS, NEEDS_CONTEXT, or BLOCKED. Include changed files, what behavior changed, check commands and observed results, checks not run, deviations, open risks, and reusable processes/artifacts. Do not say tests passed when they were skipped, timed out, or never run. A parent must inspect the diff and evidence before accepting a worker's DONE.
