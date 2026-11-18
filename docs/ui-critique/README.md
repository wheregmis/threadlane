# Design Critique: Threadlane desktop 0.1.53

Reviewed 2026-11-18 on the freshly rebuilt app (release build, dark theme, 1180×813 window) against the live `Personal` space. Screenshots are next to this file. Quality bar: [Noop](https://github.com/huacnlee/noop).

## Overall Impression

The bones are good: a calm dark palette, a clear sidebar, a task timeline that reads like a story, and solid accessibility labels on controls. The biggest opportunity is layout robustness and button hierarchy. Two layout bugs hide content on the project board and New project screen, the sidebar jumps under the cursor, and every screen mixes three button styles with no clear primary action.

## Usability

| # | Finding | Screen | Severity | Recommendation |
|---|---------|--------|----------|----------------|
| U1 | The `Failed` column is cut off at the board's right edge, next to the Repositories / Agents here / Machines panel, so failed tasks, the ones that need attention most, are hidden. (On inspection it isn't an overlap: five columns need 792px but get about 676, and the board scrolls sideways with no visible scrollbar.) | [project-board](project-board.jpg) | 🔴 Critical | **Fixed:** adjacent empty columns now stack in one grid cell, so a typical board fits beside the panel. |
| U2 | The sidebar's **In progress** row appears and disappears as runs start and stop, which shifts every item below it by about 38px. A click aimed at `threadlane` landed on `New project`. | sidebar | 🔴 Critical | Reserve the slot (keep the "In progress" header with an empty state), or show running work as a badge on the project row instead of a separate row. |
| U3 | On **New project** the status bar (`Answered Personal / D-0005…`) sits right under the form instead of at the bottom of the window. The page does not fill the height. | [new-project](new-project.jpg) | 🟡 Moderate | Give the page body `flex-1`/full height like the other screens so the footer stays pinned. |
| U4 | Empty Queued / Running / Waiting columns get the same width as `Done`, which holds 20 cards squeezed to about 4 words per line. | project-board | 🟡 Moderate | Collapse empty columns to a narrow header with a count, or let columns size to their content. Show Done as a compact list or "last 5 + show all". |
| U5 | Home is a near-empty page: one running row, two text links, and a composer floating mid-screen. "Show conversation history · New reply" does not look clickable. | [home](home.jpg) | 🟡 Moderate | Make Home answer "what needs me?": waiting-on-you decisions, failures, then running work. Pin the composer to the bottom as in Main Chat. Render links as buttons or underlined links. |
| U6 | The Home "Running now" table has a `Machine` column that is empty for the only row. | home | 🟢 Minor | Fill it in (`hub`) or drop the column when there is one machine. |
| U7 | Tasks list is ordered oldest first (T-0002 at top). It has no column headers, filter or status grouping. 20 identical green `Done` pills are noise. | [tasks](tasks.jpg) | 🟡 Moderate | Newest first. Group by status (Needs you / Running / Failed / Done) or add a status filter. Keep pills for the non-done states only and dim done rows. |
| U8 | Agents mode matrix is clipped on the right (`Backend Develope…`, Frontend missing) with no scroll hint. `—` cells look like buttons but it is unclear what they do. | [agents](agents.jpg) | 🟡 Moderate | Transpose it (agents as rows, projects as columns), which fits far more agents. Use a select/segmented control with a visible label instead of `—`. |
| U9 | Space settings shows the ceiling twice: as the read-only row `Ceiling: Assisted` and as the segmented control right below. Pacing values (`Off`) are shown read-only with one shared `Edit pacing` button. | [space-settings](space-settings.jpg) | 🟢 Minor | Drop the read-only row. Make each pacing value edit inline. |
| U10 | Machines page title is **Spaces and machines**, while the nav says *Machines*. It also contains a full `+ Add space` form, which duplicates the sidebar's `Add space`. | [machines](machines.jpg) | 🟡 Moderate | One destination per job: Machines lists machines and pairing, and Add space opens its own screen or sheet. The title should match the nav label. |
| U11 | Destructive actions sit beside routine ones at equal weight: `Archive project…` next to `Pause project`, `Unpair this app…` under `New join code`, `Cancel…` next to `Pause` on a routine. | board, machines, [routines](routines.jpg) | 🟡 Moderate | Move destructive actions into `More` or style them as quiet/danger. Keep the confirm dialogs. |
| U12 | Chat: `Open threadlane` buttons float above messages with no clear link to which message they belong to. Speaker names vary between `Lead` and `threadlane lead` for what reads as the same role. The text "No message sent." appears as a message body. | [main-chat](main-chat.jpg) | 🟡 Moderate | Attach the project chip to the message header (`Lead · threadlane · 12:30`). Use one speaker name. Hide or rephrase no-op messages. |
| U13 | Status bar text `Answered Personal / D-0005: start.` is cryptic and is the same on every screen. | all | 🟢 Minor | Plain language with a time: "Lead answered decision D-0005 · 2 min ago", and make it clickable. |

## Visual Hierarchy

- **What draws the eye first**: on Tasks, the full-width solid white `New task` bar is the heaviest element on the page, heavier than the data. On the project board the small white `New task` pill wins, which is right. Elsewhere nothing leads, because every action is a ghost button.
- **Reading flow**: the sidebar and page titles read well. Inside pages the flow breaks where secondary metadata (`Ceiling · Assisted  Lead mode · Assisted`, `Chief of Staff · hub · codex · started 13:19 · took 1 min`) uses the same grey as body text, so it competes with titles.
- **Emphasis**: Routines sets the whole prompt in bold as its title. A 5-line bold paragraph hides the useful facts (`every 60 minutes · Next 13:38`, `Active`). Use a one-line name or the first sentence as the title, show the prompt in regular weight and collapsed, and put the schedule in the header.
- **Whitespace**: generous on Home and New project (too empty), cramped on the board's Done column and the Agents matrix. Agents cards repeat `codex` / `Runs on hub` on every card, and the slug repeats the name. Show only what differs from the default.

## Consistency

| Element | Issue | Recommendation |
|---------|-------|----------------|
| Buttons | Three unrelated styles share the same role: solid white (`New task`, `Create project`, `Connect`), outlined dark (`Pause project`, `New join code`), and bare text (`Attach images…`, `Show conversation history`, `Open threadlane`, `Activity`). | Define primary / secondary / ghost / danger and use one primary per screen. |
| "New task" | Full-width bar on Tasks, small pill with icon on the project header. | Same component and placement (page header, right) everywhere. |
| Settings icons | Space `Settings` and `App settings` use the same gear. | Different icons, or label the first `Space settings` to match its page title. |
| Toggles | App settings mixes switches (notifications) and a checkbox (hold messages) for the same kind of boolean preference. | Switches for all of them. |
| Numbers | `241900 input tokens` is unformatted. Chat and task bodies show raw ISO timestamps (`2026-11-18T18:19:47Z`) and full SHAs. | Thousands separators. Local relative times. Short SHAs as links. |
| Page structure | Some pages wrap content in a bordered card (Space/App settings, Machines), others are flat (Tasks, Routines, Agents). | Pick one. Noop keeps flat sections with dividers. |
| Tabs | The task detail shows a lone `Activity` tab styled as a ghost button. | Hide the tab bar when there's only one tab, or style it like the project tabs (`Tasks · History · Agents…`). |
| Truncation | Sidebar running row `threadlane · l…` cuts off the useful part (the agent). | Show the agent name (`Lead`) with the project as a secondary label, or truncate in the middle. |

## Accessibility

- **Color contrast**: primary text on near-black passes easily. The secondary grey (labels like `Schema version`, timestamps, `Personal` subtitles, column headers) looks to be around #7a7a7a on #0b0b0b, roughly 4.5:1, which is borderline for 12px text. Status-bar text and the board's per-card metadata look dimmer still. Measure these, and raise the secondary token to about #8e8e8e (≥5:1).
- **Selected state**: the selected option in the ceiling segmented control and the selected sidebar row differ only by a slightly lighter fill. Add a stronger fill or an accent and a check icon.
- **Targets**: sidebar rows (34px) and header buttons (24px tall) are fine for pointer use. The 18px-tall task list rows and the `Reply` links (24px) are tight. Aim for ≥28px row height.
- **Screen readers**: good. Controls expose meaningful labels (`Set the ceiling of Personal to Off`, `New agent in Personal`, `Pause routine 3`, `Copy T-0022`). Keep that up as components change.
- **Text readability**: 12–13px body in chat is OK. Long chat messages run the full width, about 120 characters per line. Cap the message width around 72ch.

## What Works Well

- The **task detail** is the strongest screen: breadcrumb, status + copyable ID, a timeline that tells what happened, and a Changes/Related/Usage side column. It is the template the other screens should follow.
- The sidebar structure (space → its pages, then app-level items) is clear, and keyboard shortcuts show in hover tooltips (`Main Chat ⌘4`).
- Status colours are restrained and meaningful (green done, red failed, blue running). The palette is calm and close to Noop's.
- Empty states use helpful sentences ("No apps are paired over the network. Use New join code to pair one.").
- Accessibility labels on controls are thorough.

## Priority Recommendations

1. **Fix the two layout bugs (U1, U3) and the sidebar jump (U2).** They hide content and cause misclicks, and each is a small layout change. U1 is the most urgent because it hides failures.
2. **Introduce a button hierarchy (primary/secondary/ghost/danger) and apply it.** It fixes the Tasks page's dominant bar, the plain-text links that don't look clickable, and destructive actions sitting at equal weight (U5, U11, the consistency table) in one pass.
3. **Make Home and Tasks answer "what needs me?"** Put waiting-on-you, failed and running work first, newest first, with done work collapsed (U5, U7, U4). This is where users decide what to do next.
4. **Tighten information density.** Drop repeated metadata (Agents cards, double ceiling, `hub · Hub`), transpose the mode matrix, and format numbers, times and SHAs.
5. **Raise the secondary-text contrast token** and strengthen selected states.
