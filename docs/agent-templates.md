# Starter agents

A new hub has one agent, the Lead. This adds a small set of ready-made
agents a person can add with one command or one click, adapted from
[Noop's templates](https://github.com/huacnlee/noop/tree/main/templates/agents)
(MIT). Built by Threadlane itself, as tasks on the `dev` hub.

## 1. What a template is

A file `templates/agents/<id>.md` in this repository, compiled into the
hub: a first line `# <Name>: <one-line role>`, a `Backends:` line (`claude`,
`codex`, or both in order), and the rest is the agent's instructions.
Nothing else. A template is only a starting point: adding one creates an
ordinary agent whose instructions the owner can edit; later changes to
the template do not touch agents already created.

## 2. The templates

Noop's five, rewritten for how Threadlane works (tasks, decisions, the
memory tools, routines), not copied:

- **Chief of Staff.** When the team is idle, offers the owner a short
  menu of concrete next actions and turns the chosen one into tasks.
  - Never asks an open question. Gathers candidates itself and never
    invents them: unfinished, failed and stuck tasks from the progress
    files and task index; goals and focus in memory (`project/notes/`,
    `space/notes/`); events that were recorded and not acted on; chores
    a routine could take over; one ambitious option.
  - Asks once with `ask_user`: four to six options (the tool's limit),
    each "title: expected outcome, effort, why now", its recommendation
    first.
  - On the answer, creates one task per chosen item with a precise goal
    and a "done when", and finishes. An option offered twice and not
    picked is noted in memory so it is not offered again.
  - Runs as a task: a routine ("weekdays at 09:00") tells the lead to
    start it, or the owner asks for it.
- **Engineer.** Writes and changes code in the task's worktree, runs the
  project's checks in the foreground, keeps its progress file, finishes.
  (The `dev` hub's hand-made Engineer becomes this template.)
- **Reviewer.** Reads a task's branch, reports defects with file and
  line, changes nothing.
- **Reporter.** Writes a daily or weekly summary from finished tasks,
  decisions and the lead's messages into `project/notes/reports/`, and
  tells the lead it is there. Never writes to an outside system.
- **Architect.** Curates how the team works: distils repeated answers and
  corrections into project notes, proposes (never applies) changes to
  instructions. Overlaps with dreaming on purpose: dreaming is the
  scheduled, tool-restricted form; this is the on-request one.

Noop's Apprentice (learning the owner's taste from their own AI sessions)
is left out: it needs to read the owner's local Claude and Codex history,
which this system does not touch.

## 3. Using one

- CLI: `threadlane agent templates` lists them; `threadlane agent add
  --template chief-of-staff [--name Tudi]` creates the agent from it.
  Binding it to a project is `threadlane mode <agent> on_request -p
  <project>` as today.
- App: "New agent" offers "Start from a template" with the list and the
  first lines of each; choosing one fills the form, which can be edited
  before saving.
- A new hub still starts with only the Lead.

## 4. How it is built

Two tasks for the Engineer in the `threadlane` project, in the
`threadlane-local` repository, with no dependency between them:

1. The template files, the loader compiled into the hub, the client
   request that lists them, `agent templates` and `agent add --template`
   with tests, and the README section.
2. "Start from a template" in the app's new-agent form, state layer with
   tests against the fake hub, after task 1.

Then, on the real hub: add a Chief of Staff from the template, bind it
to `threadlane`, ask the lead to run it, and see whether the menu it
offers is real and its tasks sensible. What that shows goes in
`docs/dogfood.md`.
