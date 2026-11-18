# The best experience in the app

The plan for the UI quality pass (`Plan.md`, "Before the prerelease: the
best experience in the app"). The screens exist and are wired to the hub;
this pass is about how good they are to use. Built in stages by Codex,
each checked on screen, not only by tests.

## 1. What the Noop study found

[Noop](https://github.com/huacnlee/noop) was read from its source and
screenshots on 2026-11-17; it was not run. Two facts to keep in mind:

- It is not a GPUI app. It is TypeScript on NSH, a framework that mirrors
  GPUI Kit's vocabulary. Its ideas and measurements transfer; its code
  does not.
- Its model is one team channel plus tasks. Ours is spaces, projects and
  a board. Its channel corresponds to our Home.

What it does well, and we do not yet:

- **A quiet, near-monochrome look** where colour means status only
  (running blue, waiting yellow, done green, failed red, idle grey), with
  icons, a radius and type scale, and tabular numerals on every time.
- **Decisions as cards in the timeline**: one option per line as a full
  sentence, the recommended one tagged, "Other instructions…", and an
  answered card that stays as a record with a check.
- **Answering from the keyboard**: 1 to 9 pick an option, J and K step
  between pending decisions, R replies, `?` lists the shortcuts.
- **Running work at a glance**: a status dot, a title and elapsed time on
  every row; on a task, one live line ("Latest: step · 2 min ago") that
  turns amber when quiet, and why a queued task waits.
- **Rendered markdown** in messages, with task ids as links.
- **A composer that forgives**: Enter sends, Shift+Enter breaks, a held
  send can be undone, a failed send restores the text, slash commands.
- **Friendly empty, loading and error states**, a "Live" dot, and a
  reconnecting banner instead of raw errors.
- **A sidebar that is the inbox**: needs your decision, in progress,
  queued, recent, then machines.

What it lacks, which we should not copy: no search, no command palette,
everything in one file, and only a "latest step" line for long runs.

## 2. Stages

Each stage is one Codex job in its own worktree, verified with the three
checks and then looked at on screen against a hub with real history.

**Stage 1: the look.** One tokens module (radius, type, spacing, control
sizes, the status palette with tints, light and dark) used by every
screen; an icon set for the sidebar, statuses and buttons; a status
indicator and a count badge as shared components; tabular numerals on
times and counts; the selected sidebar row as a soft fill. No behaviour
changes. This comes first because every later stage builds on it.

**Stage 2: decisions and the keyboard.** The decision card rebuilt on the
Noop pattern for all three kinds (approval, proposal, question), with the
answered state kept as a record. Keys: 1 to 9, J and K, R, G for latest,
`/` to focus the composer, `?` for a shortcuts dialog, and the menus'
existing shortcuts listed there. Single keys only when not typing.

**Stage 3: reading and writing.** Markdown rendering in messages and
results, with task and decision ids as links and selectable text. The
composer: Enter and Shift+Enter, a failed send restoring its text, a
held send that can be undone, and a reply to a specific message. Day
dividers, a "new messages" line and "jump to latest".

**Stage 4: running work and the task page.** A status dot and elapsed
time on every running and queued row, in the sidebar too. The task
header as badge, id, title and one meta line; a live "latest step" line
with the quiet warning; the step log behind a toggle. This needs the hub
to return a run's latest step, which it records in the journal.

**Stage 5: states and the edges.** Empty, loading and error states
written as plain sentences on every screen; a connection indicator and a
reconnecting banner; confirm dialogs where an action cannot be undone;
right-click menus on sidebar rows and cards; tooltips with shortcut
hints; the machine page with load and tools.

## 3. Not in this pass

Attachments and images in the composer, voice input, slash commands the
user can edit, a second language, the phone. Search and a command
palette are worth having and Noop has neither; they are a later stage
once the above is in.

## 4. How each stage is judged

A stage is done when the three checks pass, the screens it touched have
been looked at in light and dark against the `dev` hub's real history,
and what was found is fixed or logged in `docs/dogfood.md`.
