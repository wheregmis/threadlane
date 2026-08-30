# GitHub Agent Workspace

## Goal

Turn a Git-backed Threadlane project into a clear issue-to-PR workspace where several agents can work in parallel without hiding repository state or publishing anything on the user's behalf.

## Product Shape

Add a project-scoped **GitHub** page with two top-level tabs: **Issues** and **Pull requests**. The page uses a native master-detail layout: a virtualized work-item list owns the main area and the existing resizable right panel shows the selected issue or pull request. Opening the linked agent returns to chat while preserving the selected GitHub item.

The shell must stay quiet and desktop-native. Reuse Threadlane's current typography, spacing, buttons, tabs, tooltips, empty states, `ListState`, and right-panel behavior. Do not imitate GitHub's web chrome or add a browser-like navigation stack.

### Issues

The issue list supports open/closed state, a text query, refresh, labels, assignee, and pagination. Each row shows the issue state, title, number, author/time, compact labels, and a local linked-task status when one exists.

Selecting an issue shows its title, metadata, Markdown body, timeline comments, and linked Threadlane tasks. The primary action is **Start agent task**. Starting again is allowed and creates another independent session; GitHub assignment is not changed.

### Pull requests

The pull-request list supports open/closed/merged state, a text query, refresh, and pagination. Each row shows draft/state, title, number, author, head/base branches, checks, review status, and its linked Threadlane task when the branch matches.

Pull-request detail reuses the existing PR summary and local review capabilities, organized as **Summary**, **Timeline**, and **Code**. Summary owns metadata and checks. Timeline owns conversation and review activity. Code owns the remote file list/diff plus pending review comments. If remote Code is not available, the local worktree Changes view remains the fallback.

## Issue-to-Agent Lifecycle

`Start agent task` opens a compact confirmation sheet with the issue, selected model, reasoning effort, and proposed branch. Isolation is mandatory for this action. The branch format is `issue/<number>-<slug>` with collision-safe suffixing through the existing worktree/session path.

Confirmation creates an ordinary durable Threadlane session and worktree, not a supervisor task or a new workflow database. The initial prompt includes the canonical issue URL and uses the existing virtual issue read mechanism. Remote issue text is untrusted context, never an instruction source.

An issue-start failure is fail-closed: if the worktree or branch cannot be created, no run starts against the main checkout. Ordinary non-issue session creation keeps its current behavior unless separately changed.

The session stores one repository-qualified link:

```text
IssueRef { host, owner, repo, number, url }
```

The link is persisted through the existing append-only session metadata/fact path and survives restart. One issue may link to multiple sessions. Pull requests remain derived from the session branch and GitHub; Threadlane does not mirror GitHub workflow state locally.

## Parallel Orchestration

The sidebar projects every durable session into one of these user-facing states:

- **Needs you**: a visible permission, conflict, failed publish, or other blocking decision.
- **Working**: the agent has an active run.
- **Ready**: the run ended with local work ready for review or publication.
- **Idle**: no active or blocking work.

Needs-you items sort before working items, which sort before the existing chronological groups. Clicking an item selects that session and exposes the normal permission or recovery controls. A hidden session must never wait on an invisible permission prompt.

This is a projection of the existing session runtime and durable harness records. It is not another state store. Inactive-session events must update the projection immediately and retain enough durable evidence to rebuild attention after restart.

## Draft and Publish Boundary

Agents may prepare local commits, PR title/body text, issue or review replies, and code-review suggestions. Every external mutation remains a user action:

- **Publish branch** pushes the exact selected worktree branch after a preview.
- **Create draft PR** shows editable base, title, and body before creation.
- **Post comment** and **Reply** publish an editable local draft.
- **Submit review** shows all pending inline comments and requires Comment, Approve, or Request changes.
- **Ready for review**, **Merge**, issue edits, labels, and assignees require their own explicit confirmation if added.

Draft PRs are externally visible and therefore count as publication. AI-generated text is marked **Draft · Not published** until the user posts it. A failed publish keeps the draft intact. An uncertain write is re-fetched before retry so the UI does not duplicate comments, reviews, or PRs.

Agents must not bypass these boundaries through automatic workflow actions. Existing generic shell permissions remain visible; the GitHub workspace itself never auto-runs `push`, PR creation, comments, reviews, merges, or issue mutation.

## State and Ownership

### `threadlane-git`

Own structured GitHub DTOs and `gh` command execution for repository identity, issue/PR queries, details, timelines, diffs, checks, and explicit mutations. Keep short-lived in-memory caches only. Commands are non-interactive and receive the GitHub credential already managed by `threadlane-auth` when available, while preserving normal `gh` credential lookup.

Read operations may retry safely. Write operations return stable remote IDs and are not blindly retried after an ambiguous failure.

### `threadlane-session`

Own durable `IssueRef` linkage through the existing session record/fact mechanism. The coding harness remains the source of run, tool, permission, and recovery truth.

### GPUI `AppState`

Own only project-page selection, filters, fetched page snapshots, local drafts, and the derived per-session attention view. GitHub data is refreshable remote state; drafts are local UI state until explicitly published.

### Views

The workspace owns page switching and the resizable master-detail shell. A focused GitHub view owns issue/PR list and detail state. The existing right panel remains the single owner of repository review actions; new remote sections extend it rather than duplicating Git controls elsewhere.

All repository actions use the selected session's `runtime_work_dir`. The canonical attached-project directory is used only for repository identity and discovery. Stale async responses are discarded when project, item identity, query revision, or selected session changes.

## Authentication and Failure States

Before loading GitHub data, resolve the repository and verify `gh` access. A stored Threadlane PAT and `gh` CLI login are both supported through one command environment policy. Tokens are never rendered, persisted in session records, or logged.

The UI distinguishes:

- not a Git repository;
- no GitHub remote;
- GitHub authentication required;
- insufficient repository permission;
- rate limited or offline;
- worktree creation failure;
- stale branch/head before publication;
- checks, conflicts, or required reviews blocking merge.

Every state provides a direct recovery action where one exists. Loading, empty, error, and stale-refresh states preserve the current selection and drafts.

## Keyboard and Accessibility

Lists support arrow navigation and Enter to open. Tabs use normal tab semantics. Every icon-only control has a tooltip and accessible label. Focus remains visible, destructive/publishing actions use text labels, and status never relies on color alone. The detail panel collapses below the list on narrow widths instead of hiding its actions.

## Performance

- Fetch only the visible page and selected detail.
- Virtualize issue, PR, timeline, and file lists.
- Debounce query changes and cancel or reject stale work.
- Reuse one repository snapshot per project and the existing 30-second PR cache discipline.
- Never format full diffs or timeline JSON during render; retain lightweight row descriptors.
- Run `gh`, Git, Markdown parsing, and diff work off the GPUI thread; every applied async result calls `cx.notify()`.

## Validation

- Unit-test GitHub JSON parsing, pagination, repository identity, status aggregation, and mutation argument construction without network access.
- Unit-test `IssueRef` persistence, restart hydration, branch naming, and issue-start fail-closed behavior.
- Test stale async-result rejection and attention projection independently from rendering.
- Add focused GPUI tests for list selection, start-task confirmation, draft retention after failure, and visible publish boundaries where the current harness permits it.
- Run focused crate tests, `cargo check -p threadlane-gpui`, and `git diff --check`.
- Run and observe the exact-source debug app before claiming visual verification.

## Deliberate Limits

Do not add a local GitHub database, GitHub Projects or milestone management, a second agent/task scheduler, automatic issue closure, automatic assignment, background execution after Threadlane exits, fork administration, or an embedded browser. Add any of these only after the issue-to-draft-PR loop proves a concrete need.
