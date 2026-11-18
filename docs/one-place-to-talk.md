# One place to talk

`Plan.md`, "Before the prerelease: one place to talk", and dogfooding
finding 13 (the General lead said it could see only General). Two
stages; each leaves the three checks passing.

## Decisions

1. **The space lead is the General project's lead.** No new agent and
   no new project kind. Its runs get, in their appended instructions, the
   list of the space's projects (name, description, whether paused or
   archived) and, per project, the open tasks' titles and statuses and
   its lead's latest message. Nothing from another space, ever.
2. **It hands goals to project leads with one new tool,**
   `ask_project_lead { project, message }`, offered only to the General
   lead. It inserts a `user_message`-like event into that project's lead
   lane with origin `space_lead`, which counts as requested work when the
   General run itself was requested. The project lead answers as usual;
   its messages in reply to a space-lead message are also delivered to
   the General lane as an event, so the space lead can report back. It
   never acts in another project's lanes otherwise, and never reads
   another project's memory (invariant 12 stands).
3. **The owner still talks to one lead at a time;** the General lead is
   the default target of Home's composer.
4. **Home becomes one timeline**, newest first, of every conversation in
   every paired space: each entry names its space and project and opens
   it. Filters by space (exist) and by "needs me" stay. The per-project
   conversation is unchanged. This is app-only; each hub still knows
   nothing of another.
5. **No lead above all spaces.**

## Stage 1: the space lead (hub)

The appended space summary for the General lead, byte-stable while
nothing changes; the `ask_project_lead` tool with the gate (tier 1,
requested when the asking run was), refusing an archived or unknown
project; replies routed back; tests for each, for invariant 12, and an
exit scenario "the owner asks General for a goal that belongs to a
project; the project's lead does it and General reports back".

## Stage 2: the merged timeline (app)

Home's timeline across spaces from the existing conversation requests,
paged; each entry with space, project and time, opening its
conversation; the composer defaulting to General; state-layer tests.

## After the current tasks land

Use [Noop](https://github.com/huacnlee/noop) as a reference for the
interaction: give the team a goal in chat, see what it accomplished, and
get a clear decision when the team needs you. Threadlane uses a space
switcher to keep each team's work and conversation together.

Each space has one Main Chat with its Lead. Open the space to pick up that
conversation and give the Lead work for any project in the space. The
composer sends to the Lead without a project picker. The Lead routes work
to the right project and reports back in the same chat.

Agents, routines, and tasks are reached from the space. Task and routine
rows can name or filter by project when it helps identify the work. Use
space-level settings for agent behavior and scheduling. Project screens
keep project details such as repositories and history, with links to
relevant work in the space.

Keep existing history and project context when changing navigation.
Project and repository permissions still apply when the Lead routes work.
Verify with two spaces and at least two projects in one space. A request
from Main Chat should reach the right project, appear in the space's task
list, and report back to the same chat. Switching spaces should open that
space's own conversation, agents, routines, and tasks.

### One task, more than one agent

The Engineer owns implementation. The Reviewer owns the independent
review. Today each task has one assignee, and the Reviewer template
expects a separate task. Keep one card for the owner's request, with
implementation and review shown as distinct steps in its timeline.

The space selects its review agent. When implementation is ready, the
Lead sends that exact result for review. The Reviewer uses a separate
checkout for repository work and returns findings with file and line.
The Engineer addresses findings on the original task. Review checks the
new revision before the task is shown as done. If a space expects review
and the Reviewer is unavailable, the task shows that it is waiting for
review. Existing publishing approvals still apply.

The runner records the implementation commit alongside each merge check.
A review assignment lists full commit IDs for its repositories. The
runner opens those commits in detached worktrees for the review and
keeps the Engineer's branch available for requested fixes.

To enable review for a space, set `reviewer = "Reviewer"` under `[space]`
in its `hub.toml`. Use the name of a separate agent in that space.
The task timeline records submissions and verdicts. Repository reviews
run on the machine that submitted the implementation so the Reviewer
can inspect local commits before they are published.

When Threadlane opens a pull request, the description names the opening
agent and task. For each completed review of that PR, the hub uses its
configured GitHub CLI login to post the verdict, feedback, and reviewing
agent on the submitted commit. GitHub shows these as comment reviews so
agents sharing the PR author's login can still report their verdicts.
Delivery retries if GitHub is unavailable or the commit has yet to be pushed.

Verify the whole path from Main Chat: an Engineer produces a change, the
Reviewer requests a fix, the Engineer updates the same task, and the
Reviewer approves the updated revision. The Lead reports the outcome in
the originating space's chat. The task timeline should identify each
agent and the revision reviewed.
