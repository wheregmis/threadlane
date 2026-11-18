# Built-in agent workflows

Threadlane ships four on-demand workflow skills, inspired by
[Superpowers](https://github.com/obra/superpowers/tree/8ca22dba9a94f28898bbce59f2537ff4d87c747d/skills)
and adapted to native tools and durable worker lanes:

| Skill ID | When to use |
| --- | --- |
| `threadlane-planning` | Before nontrivial implementation or assigning edits to a worker |
| `threadlane-debugging` | Before fixing a bug, test failure, or unexpected behavior |
| `threadlane-executing-plans` | While implementing an agreed task plan |
| `threadlane-verification-review` | Before accepting results, reporting completion, or addressing review feedback |

Only descriptions enter the skill catalog. Full bodies are embedded in the
binary and loaded through `load_skill`; no download, installed plugin, or
workspace file is required. They appear in the existing skill inventory and can
be disabled per project like other skills. Discovered user/project skills with
the same ID take precedence over built-in fallbacks. Disabled IDs are excluded
from the model catalog and rejected by the loader. Existing live-session refresh
semantics still apply; a new session fully reflects toggles and overrides.

## Plan first, then delegate

Normal native agents with `subagent` and Fusion leads receive the same plan-first
contract. Before spawning or reviving an implementation worker, inspect the code,
settle the design, and provide:

1. Goal, acceptance criteria, non-goals, and constraints.
2. Established findings and paths/symbols; label unverified assumptions.
3. Owned files, existing helpers, interfaces, and ordered implementation steps.
4. Dependencies, focused regression cases, exact check commands and expected outcomes.
5. Stop conditions, expected report, and any authorized delivery boundary.

The short `update_plan` milestones track progress; they do not replace the task
brief. Put the relevant plan in `task`/`instructions`, not only in the parent's
conversation. A plan-file reference requires checking that the file is available
in the child's actual checkout; an uncommitted file is not automatically present
in an isolated worktree. `context_refs` supply evidence, not design decisions.

Read-only research can precede a settled design with explicit questions and a
no-edits boundary. Tiny direct edits do not require formal planning. Follow-up
briefs carry changed scope and decisions, not a repeated full transcript.

New native worker prompts ask for `NEEDS_CONTEXT` instead of guessing when an
implementation brief is insufficient. Workers implement and self-review; the
lead owns final diff review and acceptance. Evidence distinguishes passed checks,
failures, skipped checks, and environmental blockers. Preserved Fusion contracts
are restored verbatim, not rewritten during revival.

## Limits and precedence

These are model instructions, not a runtime proof that a plan is good or that
the model obeyed it. No arbitrary plan-length validator rejects legitimate tasks.
Tool permissions and existing lane constraints remain the enforcement boundary.
Custom base system prompts retain their replacement semantics; the normal
default workflow guidance does not override them. Fusion and native child-role
contracts still apply where those modes already inject their own instructions.
External ACP agents own their own prompt/workflow behavior.

## Waiting for PR events without model polling

Native Git sessions expose `github_pr` with a `wait` action. Read `feedback`
first, handle existing findings, then pass its `watch_cursor` as `cursor` to
`wait` (with the same explicit `repository` and PR `number`). The tool stays
pending while host code checks the complete, paginated CI/review snapshot every
60 seconds. Unchanged polls do not return to the model or append tool messages.
Changes between the initial feedback read and registering the wait are detected
by the cursor. Array ordering alone does not trigger a wake.

The response has `wake_reason: changed` or `closed`, full feedback, and the next
`watch_cursor`. A `timeout` response contains no cached readiness snapshot;
fetch fresh feedback before claiming readiness. `timeout_seconds` defaults to
1800 and accepts 1–1800 seconds; use the remaining no-progress budget rather
than resetting it on each wait. Omitting the cursor baselines the current state.
Errors (including rate limits or incomplete pagination) return to the agent
instead of being mistaken for an unchanged or green PR. Existing credential
handling and GitHub request throttling still apply. Waiting is read-only.

This is local monitoring, not a hosted webhook endpoint: no public listener or
GitHub App installation is required. Stop/cancellation drops the asynchronous
wait and prevents further polls; an already-running GitHub read can finish.
The session remains in a pending tool call, and quitting Threadlane interrupts
it. It is not a durable subscription that wakes a stopped application. External
ACP agents without this tool retain the bounded-polling fallback.

Disabling a workflow skill removes its optional detailed guidance, not the native
delegation contract. No new approval, publication, merge, or cleanup permission is
granted by loading a skill. The upstream scripts, mandatory every-turn loading,
and automatic branch-cleanup behavior are deliberately not imported.

Adaptation attribution and the upstream MIT license are retained in
`crates/threadlane-skills/src/workflows/NOTICE.md`.
