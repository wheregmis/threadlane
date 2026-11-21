# UI/UX review: Threadlane desktop 0.1.54

Walked on 2026-11-21 against the live dogfood hub (Personal + Work spaces,
272 tasks, 2 running). Screens: Inbox, Chat, Tasks (list, graph), project
(chat, board), task page, Usage, Team (topology), Routines, Machines, App
settings. Read only; nothing was approved, sent or changed.

Builds on `docs/ui-experience.md` (the Noop-based stages). Items below are
what is still visibly missing or wrong on screen.

## Overall impression

The look is already calm and consistent: near-monochrome dark, colour for
status, tabular ids, a clear sidebar. The biggest gaps are **orientation**
(where am I, what needs me) and **density control** (long lists and long
decision cards with no way to skim). The status bar line "Answered
Personal / D-0293: approve." stayed on every screen for the whole walk.

## Usability

| Finding | Severity | Recommendation |
|---|---|---|
| Inbox "Needs you" shows only Running tasks; pending decisions live in project chat as long cards. The sidebar has no count of what waits on you. | 🔴 | Put a "Waiting on you" group first in Inbox with one-line decision rows (id, question, project, age) and a count badge on the Inbox nav item. |
| Decision cards are very long (full PR body, 19-file diff list inline) before the approve/decline buttons. | 🔴 | Lead with question + options; collapse body after 3 lines and the file list behind "19 files changed". Keep options visible without scrolling. |
| Project tab bar is six unlabeled icons. | 🔴 | Add text labels (Chat, Board, Team, Routines, Notes, Settings) or at least tooltips + an underline on the selected tab. |
| Task page: one lone "Activity" tab; timeline is two lines while "Changes / Related / Usage" fill a narrow right column that is cut off by the composer. | 🟡 | Drop the single tab; give the right column its own scroll or move it above the composer fold. |
| Task page "Latest: Called Bash · 2 min ago" is low-contrast and reads as metadata, not live status. | 🟡 | Render as the live line from stage 4: status dot, pulse while active, amber when quiet. |
| Tasks list opens on Failed (16 rows) above Running and Queued. | 🟡 | Order groups Running → Waiting → Queued → Failed → Done, and collapse Failed/Done by default beyond 5 rows. |
| Failed list mixes retries ("retry of T-0196") as separate rows. | 🟡 | Group retries under their origin task with a "3 attempts" chip. |
| Status bar message persists indefinitely. | 🟡 | Make it a toast that fades after ~5 s, with history in Activity. |
| Routines rows show 4–5 buttons each (Details, Run history, Run once, Pause, More). | 🟡 | Keep Run once + Pause visible; move Details/Run history into the row click and More menu. Truncate long routine titles to one line with the full text on hover. |
| Routine row "Recheck #555" lacks "Run once", so buttons do not align with the others. | 🟢 | Keep column positions fixed; show a disabled or empty slot. |
| Usage shows "Loading usage…" skeleton on every visit; cost "USD 272.7808" has 4 decimals. | 🟢 | Cache the last result and refresh in background; show money as `$272.78`. |
| Machines: "Unpair this app…" sits next to "New join code" with equal weight. | 🟢 | Style unpair as a destructive/secondary action and confirm it. |
| App settings: "Hold messages for 3 seconds so a send can be undone" is off by default. | 🟢 | Default it on; it protects against wrong-recipient sends. |

## Visual hierarchy

- **Eye goes first to**: the largest card in the content area (decision
  card, kanban column). Correct on Chat; wrong on Inbox, where the page is
  mostly empty and the real "needs you" item is elsewhere.
- **Reading flow**: sidebar → title → content is clear. Page headers vary:
  some pages centre content in a ~480 px column (Inbox, Chat, Usage,
  Machines, Settings), others go full width (Tasks, Team, Routines,
  project). The jump is noticeable when switching.
- **Emphasis**: secondary meta (role, machine, time) is grey and right
  aligned on board cards, which makes cards ragged. Board cards put the
  title on two lines with id inline; the id should sit on its own line
  like the graph cards do.

## Consistency

| Element | Issue | Recommendation |
|---|---|---|
| Page width | Centered column on some pages, full width on others. | One rule: lists/boards full width, reading pages centered at the same max width. |
| Page title | "Usage" in nav, "Space usage" as title; "Chat" vs "Space chat" button. | Match nav label and title. |
| Icons | Usage and Tasks use the same grid icon in the sidebar. | Give Usage a chart icon. |
| Buttons | "Archive project…", "New routine…", "Cancel…" ellipsis usage varies; "New task" is the only filled button in headers. | Ellipsis only when a dialog follows; one primary button per header. |
| Time format | "Yesterday 12:26", "2026-11-19 12:55", "15 min", "less than a minute", "Next Tomorrow 15:15", "Next 2026-11-23 04:00 · UTC". | One formatter: relative within 24 h, "Mon 12:26" within a week, date beyond; drop "Next Tomorrow". |
| Decision options | Answered card shows "approve" / "decline" lowercase. | Sentence case, verb-first labels ("Approve", "Decline"). |
| Board columns | "Sleeping" and "Cancelled" columns sit beside core states; empty "Waiting on you 0" takes a full column. | Collapse empty columns to a thin header; consider hiding Cancelled by default. |

## Accessibility

- **Color contrast**: body text passes. Grey meta text (timestamps,
  "Goes to T-0269, not the lead.", "Kept in …desktop.json", placeholder
  "decline" option) looks near 3:1 on the near-black background; check
  against 4.5:1 and lift the tertiary text token.
- **Status by colour only**: Failed/Running dots rely on red/blue; the
  chips carry text, but sidebar dots (threadlane's blue dot) do not.
  Add a tooltip and accessible label.
- **Targets**: sidebar rows are 31 px tall (fine); header buttons and
  filter chips are 22 px tall, the "+" new project is 22×22. Raise to
  28 px minimum.
- **Accessibility names**: several buttons report no title to the
  accessibility tree (space picker, project rows, Machines row, task
  rows, the project's first tab). Give every button a label.
- **Keyboard**: no visible focus ring was seen during the walk; confirm
  focus styling on chips, tabs and cards.

## What works well

- The decision card's "Recommended" tag and the answered state kept as
  a record — exactly the Noop pattern, and it reads well.
- Task graph and Team topology are genuinely useful views and look
  polished; the "Blocks 1" / "1 bottleneck" call-outs are great.
- Status chips (Running, Queued, Failed, Sleeping) are consistent across
  list, board and graph.
- The composer says where a message goes ("Goes to T-0269, not the
  lead." / "To: Personal · threadlane lead") — clear and safe.
- Lead messages are rendered markdown with linked task ids.

## Priority recommendations

1. **Make "needs you" unmissable** — decisions first in Inbox, a count
   badge in the sidebar, compact decision rows. This is the app's core
   loop and today it hides in chat.
2. **Shorten decision cards** — question and options up top, details
   collapsed. Approving should never need a scroll.
3. **Label the project tabs and unify page layout/time format** — cheap
   fixes that remove most of the "where am I" friction.

## Task list

Ordered by impact; each is one small, reviewable change in
`crates/threadlane-desktop`.

### P0 — core loop
- [ ] Inbox: add "Waiting on you" group above Running, one row per pending decision (`screens/home.rs`).
- [ ] Sidebar: count badge on Inbox for pending decisions; per-project badge instead of a bare dot (`screens/shell.rs`).
- [ ] Decision card: collapse body after 3 lines and the file list behind its summary row; keep options above the fold (`screens/decision.rs`, `components/diff.rs`).
- [ ] Project screen: text labels or tooltips on the six tab icons, selected-tab underline (`screens/project.rs`).

### P1 — orientation and skimming
- [ ] Tasks list: reorder groups (Running, Waiting, Queued, Failed, Done) and collapse long groups (`screens/project.rs` / tasks list).
- [ ] Group retries under the original task.
- [ ] Task page: remove single "Activity" tab; make right column scroll independently; style "Latest" as a live status line (`screens/task.rs`).
- [ ] Status bar: auto-dismiss notices after ~5 s.
- [ ] Routines: reduce row actions to Run once + Pause + More; one-line titles with hover full text; fixed button columns (`screens/routines.rs`).
- [ ] Board: collapse empty columns; hide Cancelled by default; id on its own line in cards.

### P2 — consistency
- [ ] One time formatter for all screens (`clock.rs`).
- [ ] One page-width rule (reading pages centered, boards full width).
- [ ] Match nav labels and page titles (Usage / Space usage).
- [ ] Distinct Usage icon (`icons.rs`).
- [ ] Ellipsis only on buttons that open a dialog; one primary button per header.
- [ ] Sentence-case decision option labels.
- [ ] Money formatted to 2 decimals; cache Usage between visits.

### P3 — accessibility
- [ ] Audit tertiary text token for 4.5:1 contrast (`tokens.rs`).
- [ ] Minimum 28 px height for header buttons, chips and the "+" button.
- [ ] Accessible labels on every untitled button (space picker, project rows, task rows, Machines, first project tab).
- [ ] Visible focus ring on chips, tabs, cards.
- [ ] Text/tooltip for colour-only status dots.

### P4 — safety defaults
- [ ] Undo-send hold on by default.
- [ ] "Unpair this app" styled destructive with a confirm dialog.
