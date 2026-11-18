# Threadlane desktop: navigation and UX proposal

2026-11-18. Follows the screen-level critique in [`../ui-critique`](../ui-critique/README.md). That one fixed layout bugs; this one questions the structure itself.

## 1. The problem in one paragraph

Threadlane's job is delegation: *you say what you want → agents work → you hear about decisions and results*. The sidebar is organised around the data model instead (space → projects → the same five concepts again). The place where work starts, **Main Chat**, sits under the project list as the eighth row. **Tasks, Agents and Routines** each exist twice, once for the space and once as a tab in every project, and nothing tells you which scope you're in. You can talk to agents in three places (the Home composer, Main Chat, and a project's *History* tab), and running work also shows up in three places (Home "Running now", the sidebar "In progress" rows, and the board).

## 2. Critique of the current structure

### What it looks like today

```
Threadlane
  Home                      ← composer + needs-you + running
  Personal ●
    In progress             ← moves to the bottom after #530
    threadlane              ← opens the board; tabs: Tasks · History · Agents · Routines · Memory
    New project
    Main Chat               ← the General project's conversation
    Tasks                   ← all tasks in the space
    Agents                  ← definitions + mode matrix for every project
    Routines                ← every routine, grouped by project
    Settings                ← space settings
  Application
    Add space
    Machines   1/1          ← also has an "Add space" form
    App settings
```

### Usability

| # | Finding | Severity | Recommendation |
|---|---------|----------|----------------|
| N1 | **The main way in is buried.** Main Chat, where you hand work to the space lead, is the 8th row, below projects and *New project*. A new user's first question ("where do I tell it what to do?") has three answers: Home, Main Chat, or a project's History tab. | 🔴 Critical | One **Chat** destination at the top of the space. Its composer carries a project chip (defaults to the current project or General), so one composer reaches every lead. |
| N2 | **The same concept at two scopes, with no scope cue.** `Tasks`, `Agents` and `Routines` in the sidebar are space-wide. The project tabs with the same names are project-only. Both look identical, so users can't predict what they'll see. | 🔴 Critical | Keep each concept in **one** place, with a filter for scope. *Tasks* is one list filtered by project. *Team* (agents) is one page, and the project view shows a short summary that links to it. |
| N3 | **A project opens on the board, while the conversation is hidden in a tab called "History".** The lead conversation is the project's main surface: it's where you ask, and where results arrive. "History" suggests a read-only log. | 🟡 Moderate | A project opens on **Chat**, then *Tasks*, *Memory*, *Setup*. Rename History → Chat. |
| N4 | **Running work is shown three times.** Home "Running now", the sidebar "In progress" rows, and the board's Running column. The sidebar copy changes height and moved rows under the cursor (U2). | 🟡 Moderate | Show activity **on the project row**: a spinner while its lanes run, an unread dot or count when there's something new. No separate In progress block. |
| N5 | **Configuration sits at the same level as daily work.** Agents, Routines and Settings sit beside Chat and Tasks even though you visit them weekly at most. | 🟡 Moderate | Put them in a **Manage** group at the bottom (Team, Routines, Space settings), or behind the space menu. |
| N6 | **The project board's side panel repeats setup.** Repositories, Agents here and Machines take 220px on every visit and squeeze the board (the root cause of U1). | 🟡 Moderate | Move it to the project's *Setup* tab. The board gets the full width. |
| N7 | **Add space appears twice.** It's a sidebar row and also a form at the bottom of Machines. Machines' title ("Spaces and machines") doesn't match its nav label. | 🟢 Minor | Add space lives in the space switcher. Machines lists machines only. |
| N8 | **Home and the space are separate places,** but Home is mostly empty and repeats the space content. | 🟡 Moderate | Make Home an **Inbox**: everything that needs you or finished, across all spaces, with answer-in-place. That's the cross-space view Plan §2.1 asks for, and it gives Home a single job. |

### What works and should survive

- Space isolation is real in the model and visible in the UI (one status dot per space). Keep spaces as a top-level switch.
- Decisions as short option lists, answered in place. They should get *more* prominent, not less.
- The task detail page (timeline, Changes, Related, Usage) is the best-structured screen.
- Keyboard shortcuts on the sidebar (`⌘1…⌘9`) carry over to the new structure.

## 3. Proposed structure

```
┌───────────────────────────┐
│ ● Personal            ▾   │  space switcher: spaces, Add space, Space settings
│                           │
│ ◉ Inbox              3    │  ⌘1  needs you + results, all spaces
│ 💬 Chat                    │  ⌘2  space lead; composer picks the project
│ ☰ Tasks                   │  ⌘3  all tasks in the space, filter by project/status/agent
│                           │
│ PROJECTS              +   │  "+" replaces the "New project" row
│ ⟳ threadlane          2   │  spinner = running, number = unread
│   threadlane-local        │
│                           │
│ MANAGE                    │  collapsed by default
│ 👥 Team                    │  agents + modes (agents × projects)
│ ⏱ Routines                │
│                           │
│ ─────────────────────     │
│ 🖥 Machines 1/1    ⚙       │  footer: machines + app settings
└───────────────────────────┘
```

**Project page:** a header with the name, ceiling chip, `Pause` and `⋯` (Archive lives there), then tabs:

| Tab | Contains | Replaces |
|-----|----------|----------|
| **Chat** (default) | The lead conversation and its composer. Proposals and decisions inline. | History tab |
| **Tasks** | Board or list toggle, full width. Empty columns collapsed. | Tasks tab + side panel |
| **Memory** | As today. | Memory tab |
| **Setup** | Repositories, this project's agent modes (one column of the Team matrix), its routines, ceiling, Pause/Archive. | Agents tab, Routines tab, board side panel |

**Rules this structure follows**

1. *One concept, one home.* Each noun has one canonical page. Other places show a summary row and a link to it, never a second editor.
2. *Work first, setup last.* Ordering follows visit frequency: Inbox > Chat > Tasks > projects > manage.
3. *Scope is always visible.* Every list shows its scope as a filter chip (`Project: threadlane ✕`) instead of relying on where you clicked from.
4. *Nothing in the sidebar changes height on its own.* Activity is a decoration on existing rows.

### Key flows, before → after

| Job | Today | Proposed |
|-----|-------|----------|
| Ask for something in a project | Sidebar → project → *History* tab → composer (3 clicks) | Click the project (opens Chat) → type (1 click), or Chat → pick the project chip |
| Answer a decision | Home or a board card → *Answer* dropdown | Inbox badge → options inline (1 click) and an OS notification |
| See why a task failed | Project → scroll the board to find Failed (hidden by U1) | Inbox shows the failure, or Tasks → status: Failed |
| Make Engineer autonomous in threadlane | Agents (space) matrix *or* project Agents tab | Team → matrix cell, or project Setup → Engineer row |
| Pause issue triage | Routines (space) or project Routines tab | Routines, filtered by project |

## 4. Research plan to validate it

The user base is small and expert (developers who delegate to coding agents), so qualitative methods and a tree test go further than surveys.

**Objectives**

1. Confirm the job ranking (ask → decide → check results → configure) and how often each happens.
2. Test whether the proposed tree beats the current one on findability.
3. Find where people expect the scope (space or project) of tasks, agents and routines to live.

**Methods**

| Method | Who | Size | What it answers |
|--------|-----|------|-----------------|
| Local navigation counts | Your own daily use | 2 weeks | How often each screen is really opened. Log `Desktop::go(screen)` to a local file, opt-in, with no content. |
| Open card sort | Developers who use Claude Code / Codex | 15 | How people group: chat, tasks, decisions, agents, routines, memory, repositories, machines, modes, ceiling. |
| Tree test, current vs proposed | Same pool, split | 2 × 15 | Success rate and directness for the 5 jobs above. |
| Moderated usability test on a prototype | Same pool | 5–6 | Whether "project opens on Chat" and the unified composer hold up in real use. |

**Interview guide (45 min)**

1. *Warm-up, 5 min.* What agents do you run today, and where?
2. *Context, 10 min.* Walk me through the last time you handed work to an agent. Where did you check on it? What made you step in?
3. *Deep dive, 20 min.* When an agent needs a decision, where do you want it? How do you think of "project" vs "everything"? Which settings would you change weekly, and which only once?
4. *Reaction, 10 min.* Show the current sidebar and the proposed one. "Where would you go to… (5 jobs)". Think aloud.
5. *Wrap-up, 5 min.* What's missing? Anything you'd never use?

**Success criteria for the proposal**

- Tree test: ≥80% success on the 5 jobs, and at least 15 points better than the current tree on "ask in a project" and "find why a task failed".
- Usability test: nobody hunts for where to type a request.
- Navigation counts: Chat + Inbox are ≥70% of screen opens. If Tasks dominates, give it the top slot instead.

## 5. Suggested rollout (each stage shippable on its own)

1. **Reorder and rename**, no new screens. Chat moves to the top of the space, Manage group at the bottom, History → Chat, the project opens on Chat, and the In progress block becomes row decorations. Cheapest and biggest win.
2. **Project Setup tab.** Move the board side panel, the Agents tab and the Routines tab into it. The board gets full width.
3. **Inbox replaces Home.** Decisions, proposals, failures and results across spaces, with answer-in-place.
4. **One composer with a project chip** across Chat and project Chat.
5. **Space switcher.** Add space and Space settings move into it, Machines and App settings move to the footer.

Stages 1–2 can ship before the research. Stages 3–5 should wait for the tree test.
