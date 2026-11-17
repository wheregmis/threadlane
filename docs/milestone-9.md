# Milestone 9 design: modes and permissions

Maps `Plan.md` §15 milestone 9 onto the crates. Read with `Plan.md` §2.3,
§9.1, §9.2, §9.4, §5.7 (one-shot questions) and invariants 7, 13, 14, 16
and 19 of §13. It follows milestone 8, whose timers back decision expiry.
It is built in three stages, in order; each leaves `cargo test
--workspace`, clippy with `-D warnings` and `cargo fmt --check` passing.

## 1. Decisions

1. **The gate becomes one pure function** in the hub: (tool and its tier,
   arguments, binding mode, project and space ceilings, overrides,
   standing rules, whether the run is tainted, whether the work was
   requested) to one of allow, review, ask, block. Everything that decides
   goes through it and it is tested as a table against the matrix of
   §2.3. Today's special cases (publishing asks, protected memory asks, an
   agent's schedule asks) become rows of it.
2. **Until now every mode above Off acted as On request.** After stage A
   the binding's mode is what the gate reads. Existing hubs keep their
   stored modes; nothing is migrated except new columns.
3. **Ceilings.** A space has a ceiling (it exists: `HubInfo.mode_ceiling`)
   and a project may set a stricter one. Setting a binding above the
   effective ceiling is refused, with the ceiling named.
4. **A standing rule's argument pattern is exact on the fields that name
   the target** (repository, and for a memory write the path) and ignores
   free text (titles, bodies). That is the smallest pattern that makes
   "always allow Engineer to open draft pull requests in Payments API"
   work. Tier 2 rules apply in any mode; tier 3 rules only in Autonomous.
5. **Review is a one-shot question to a backend with no tools** (§5.7),
   with a fixed answer format: allow or block, and one sentence. An error,
   a timeout, or an answer that does not parse is ask, never allow. It
   uses the hub machine's first available backend; Claude's `ask`, left
   unimplemented since milestone 3, is built here.
6. **Screening** is the same kind of question, asked once at the start of
   a run about its `external` event payloads: clean, tainted, or drop.
   Fail closed means tainted. A tainted run works one mode lower, minimum
   On request, for that run only.
7. **A proposal is a paused `create_task`** (§9.4): in Assisted, a lead
   run triggered by work you did not request may run, and its
   `create_task` asks. In On request such an event is recorded and starts
   no run (milestone 8 already does this). In Autonomous the task is
   created. When the lead assigns the task to another agent, both modes
   must permit it; the stricter wins.
8. **Every decision expires**, by a timer (default 24 hours, a setting),
   as declined with the reason "no response".
9. **`ask_user`** is the `input` kind: the options are the agent's, the
   answer returns as the tool's outcome. No stored call is performed.
10. **The "survives a machine change" clause of the exit is deferred**
    with milestones 6 and 7. The rest of the exit is built.
11. **The red-team scenarios are a live suite**, ignored by default,
    because their result depends on a real model. `cargo test` covers the
    mechanics (marking, tainting, step-down, the gate's answer) on the
    scripted backend; the live suite reports a measured rate.
12. **No new dependencies.**

## 2. Stage A: the gate, ceilings, overrides, standing rules, expiry

- **Core.** Tiers on every tool (they exist); the forbidden list as a
  built-in list plus space and project additions; `Mode` ordering and
  step-down; the gate function of decision 1 and its result type.
- **Store.** Project ceiling; binding overrides (a stricter cell per
  tier); `standing_rules` (the table exists): create from an answer, list,
  revoke, count uses.
- **Hub.** The gateway calls the gate for every tool call. `review` is
  treated as ask until stage C adds the classifier. Decisions gain
  `offers_standing_rule` where a rule could apply, and answering with
  "always" creates the rule in the same transaction as the answer.
  Decision expiry by timer. `ask_user`.
- **Client interface.** In `org`: set a project's ceiling, set an
  override. In `decisions`: answer with `always`, list and revoke standing
  rules. CLI: `threadlane ceiling`, `threadlane answer … --always`,
  `threadlane rules`, `threadlane rule revoke`.

Tests: the gate as a table, one row per cell of the matrix times tainted
or not, with ceilings and overrides; a rule removes the next identical ask
and a revoked one does not; a tier 3 rule has no effect outside
Autonomous; a changed argument hash still performs nothing (invariant 7);
a repository not registered in the run's project is refused before any
ask (invariant 19); expiry declines once; `ask_user` round trip; the exit
scenario "the same scenario under each mode does exactly what its row
permits" on the scripted backend.

## 3. Stage B: initiative and proposals

- **What may start.** Placement reads the binding's mode for a lane whose
  pending events are all unrequested: On request, not eligible (recorded);
  Assisted and Autonomous, eligible.
- **Proposals.** In such a run `create_task` asks (kind `proposal`) in
  Assisted and is allowed in Autonomous; the stricter of the lead's and
  the assignee's modes decides. Accepting creates the task with origin
  unrequested-accepted, which then counts as requested for its own work;
  declining resumes the lead with the note; "do not propose this kind
  again" is recorded as a project note for the lead, not as a rule in the
  gate.
- **Agent-created routines** fire unrequested events and follow the same
  rule.

Tests: each mode against an unrequested external event; the stricter of
two agents; the exit scenario "in Assisted a failing build on a registered
repository produces a proposal, and in Autonomous it produces a task".

## 4. Stage C: the remaining tools, review, screening

- **Tools.** `comment` (tier 2, on a pull request or issue of a
  registered repository), `merge` (tier 3), `create_issue` (tier 2),
  through `Forge` with the fake in tests and `gh` in the real one;
  performed by the runner after the gate, as publishing is. Deny rules
  gain their direct forms on both backends.
- **Review.** Claude's `ask`; the classifier of decision 5 wired to the
  gate's `review` result; fail closed.
- **Screening** of decision 6, and the step-down.
- **The live red-team suite** of decision 11, as ignored tests with a
  short README on how to run it and what it prints.

Tests: each new tool gated per mode, performed at most once, refused for
an unregistered repository; classifier allow, block, error, timeout,
unparseable; screening clean, tainted, drop, error; a tainted run in
Assisted asks where a clean one is reviewed (invariants 13, 14, 16); each
deny rule reaches each backend's flags.

## 5. Not in this milestone

The app's project-agents screen with the expanded "may do without asking"
table and standing rules (a UI stage alongside), anything on a second
machine.
