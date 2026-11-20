# Improving Threadlane Dreaming

**Recommendation.** First test what a fresh Lead can still do after a dream edits memory. Then pilot two kinds of learning in one project: how the owner wants work done, and which project procedures have actually worked. Keep the existing Lead, project timer and versioned notes. The research follow-up below covers scheduling, concurrent edits and how to measure the pilot.

## Implemented behavior

Each dream fixes its review list when the run starts. The hub pages up to 200 runs, 200 tasks and 200 decisions per occurrence, gives unread records priority, and carries them forward until a committed dream has seen them. A quiet project skips model work between weekly maintenance passes. Changing the cadence or re-enabling Dreaming still allows its first occurrence.

The project preference note can cite owner chat as `E-<event id>` and choices as `D-<decision id>`. Put one conditional default and its citations on each line. The Lead resolves the citations against this project's evidence before receiving the rule. A supported rule needs three different runs. Recent conflicting evidence and effective dates are passed to the Lead for judgment. Procedure notes can use task outcomes and the latest completed review. Reports proposed during Dreaming require owner approval.

Dream edits, moves, deletions and replacements use the source file's `expected_version` from `memory_view`. Creation defaults to an absent destination. The Memory tab opens the version written by a dream, including a deletion and its preceding history. The deterministic suite exercises stale writes, late arrivals, interrupted review and idle occurrences. The two opt-in live evaluations use synthetic project histories. [Evaluation commands](evals.md)

## What to borrow from Noop

| Noop mechanism | Threadlane adaptation |
| --- | --- |
| The `learn-ceo` routine gathers local Claude and Codex turns and hub interactions. The Apprentice sees the owner's correction alongside a short extract of what the assistant previously said. | Pair an owner message with the Threadlane response it corrects. Use project and conversation identity to establish context. Threadlane already made a deliberate choice to leave personal CLI histories outside its project boundary. [Collector and learning prompt](https://github.com/huacnlee/noop/blob/199c7ce9de22e0b6d34bad8b805e99e09b2003cf/src/sessions.ts) · [Threadlane starter-agent design](agent-templates.md) |
| The Apprentice distills a rule with an example, records dates and evidence, and revises an existing lesson as evidence changes. | Keep the circumstance, preferred action, exception and source IDs together in one project note. Count independent occasions so that repeating a dream never strengthens a lesson. [Apprentice profile](https://github.com/huacnlee/noop/blob/199c7ce9de22e0b6d34bad8b805e99e09b2003cf/templates/agents/apprentice/profile.md) |
| The Judge records what it would choose before the owner answers. The Architect reviews agreement and overrides by agent before proposing wider autonomy. | Borrow the measurement first. Score predictions on later decisions and examine the cases where the Lead should ask. Any future change to a project mode remains a separate owner decision. [Decision routing](https://github.com/huacnlee/noop/blob/199c7ce9de22e0b6d34bad8b805e99e09b2003cf/src/decisions.ts) · [Architect profile](https://github.com/huacnlee/noop/blob/199c7ce9de22e0b6d34bad8b805e99e09b2003cf/templates/agents/architect/profile.md) |

## Baseline before implementation

Before this change, Dreaming saw up to 20 recent owner-answered `ask_user` decisions. The session tool returned run outcomes, rationales, delivered messages and project tasks. The Lead received a clipped preference summary when Dreaming was enabled and at least three choices were available. An owner who made corrections only in ordinary chat could never establish a default through this path. [Dream tools and preference loading](../crates/threadlane-hub/src/dreaming.rs) · [Store queries](../crates/threadlane-store/src/dreaming.rs) · [Lead guidance](../crates/threadlane-hub/src/context.rs)

## Owner-correction pilot

### 1. Give the dream a bounded view of owner corrections

Extend `project_sessions` with project-local user-message events from the runs in the existing dream window. Return the event ID, run ID, time and a short text excerpt. Include a prior delivered reply only when an explicit reply reference or unambiguous same-conversation ordering identifies it. An isolated “do that again” supplies no usable lesson. Use the existing page and field budgets, strip attachments and quoted payloads, and redact sensitive strings before clipping. Only owner-authored text can support a preference. Agent replies supply context and imported issue text carries no owner authority.

The current dream can also call `report_threadlane_limitation`, which can publish to GitHub. With richer private inputs, route dream-originated reports through the existing approval path in the hub and show the exact outgoing text. Keep the private evidence IDs and excerpts in project memory. [Session tool](../crates/threadlane-hub/src/dreaming.rs) · [Page limits](../crates/threadlane-hub/src/page.rs) · [Report gateway](../crates/threadlane-hub/src/gateway.rs)

**Acceptance.** Seed two projects, a quoted outside instruction, a retry, a secret and an owner correction. The dream can associate the correction with the right reply, sees the source event once, and cannot read the other project's messages. A scripted dream attempting to report an excerpt produces zero forge writes before owner review.

### 2. Turn independent observations into a usable project default

Extend the existing `project/notes/user-preferences.md` convention. Record a short conditional rule, one paraphrased example, supporting event or decision IDs and the last confirmation date. Keep the three-run threshold. A choice and a follow-up explaining that choice count as one occasion. Repeated reads, retries, silence and the Lead's own prior behavior add zero evidence. A newer explicit contradiction suspends the affected default immediately when available to the Lead. The next dream narrows or removes it. An unrelated or explicitly one-off exception stays scoped to that occasion.

Update `lead_preferences`, the Lead guidance and prompt assembly together. Resolve cited evidence within the same project and supply a bounded excerpt for each applicable rule, including recent counterevidence after the last dream. Allow an evidenced correction-derived default through when there are zero choice cards. Preserve the current choices-only path for existing projects and older notes. Treat an unresolvable citation as insufficient support. Reuse the Memory tab's version history to inspect what changed. [Lead prompt assembly](../crates/threadlane-hub/src/begin_run.rs) · [Dreaming tests](../crates/threadlane-hub/tests/hub/dreaming.rs) · [Memory tab](../crates/threadlane-desktop/src/dreaming.rs)

**Acceptance.** Across three weekly reports the owner asks for blockers first. A fresh Lead puts blockers first on the fourth weekly report. It leaves a one-off incident brief alone. After “stop putting blockers first for weekly reports,” the next Lead stops using that default even before the next dream. Replaying the same three events produces no new evidence or memory version. Switching Dreaming off suppresses the learned context. Approval requests and proposed instruction edits still follow their existing paths.

### 3. Measure whether Dreaming learned the right lesson

Add a chronological replay to the existing evaluation harness. At each held-out decision, give the Lead only the question, options and evidence available before that decision. Record an option or “ask” before revealing the owner's answer. Compare the current choices-only behavior and the enhanced behavior on identical cases. Include a repeated preference, an unrelated context, a reversal, a qualified answer and a request that requires approval. [Evaluation harness](evals.md) · [Live memory evaluation](../crates/threadlane-hub/tests/exit/evals.rs)

Report correct predictions out of attempted predictions, attempted predictions out of eligible cases, and incorrect defaults on the cases that require asking. Show counts alongside percentages. Ship the pilot when the enhanced path handles the correction-only fixture, preserves the choice-only fixtures and makes zero unauthorized decisions in the boundary cases. Then review misses from one opt-in project's later decisions before considering a live shadow judge or wider rollout.

## Research follow-up and rollout

### First, test what survives cleanup

The live memory eval asks for recall **before** enabling Dreaming. Afterward it scores stored content and the change in checker findings. The checker compares word counts for near duplicates. “Publish release notes before tagging the build” and “Publish release notes after tagging the build” share six of seven words and score 0.857, above its 0.8 threshold. This is a valid review candidate. Deleting either rule without checking its scope could still make the finding disappear. [Live eval](../crates/threadlane-hub/tests/exit/evals.rs) · [Checker implementation](../crates/threadlane-hub/src/memory_check.rs)

Add one fixture with a real duplicate, a broken link and two deliberately similar rules for stable and preview releases. After cleanup, start a fresh Lead session with no access to the seeding conversation. Ask what to do for each release and what to do for an undocumented third channel. Pass when it recalls both orders, abstains on the third, and preserves the valid instruction beside the broken link. Use a scripted run for deterministic checks and a paired live replay with Dreaming on and off to measure behavior. Score whether the correct source reached the Lead separately from whether it interpreted it correctly. LongMemEval makes this distinction between retrieval and reading and explicitly evaluates updates and abstention. [LongMemEval](https://arxiv.org/abs/2410.10813)

### Protect an owner's edit while a dream is running

Memory calls already replay identical writes safely, and rollback appends the selected version as an owner write. But a dream can read version 3, an owner can save version 4, and a later dream delete can still succeed. The store checks the lease and that the path exists. It does not compare the version the dream read. For dream replacements, deletions and renames, check the expected version **inside the write transaction**. Check destination absence for a new file. Keep replay detection ahead of that check so a successful call can be retried. On conflict, re-read the owner's change before proposing another edit. [Memory tools](../crates/threadlane-hub/src/memory.rs) · [Versioned writes and rollback](../crates/threadlane-store/src/memory.rs)

Test the interleaving directly. An owner restores a note between the dream's read and delete. The stale delete must leave the restored version intact, and a replay of an already successful delete must produce no additional version. Link the last dream's changed paths to their before-and-after versions, including deleted notes, so the owner can inspect what happened. [Dream summary](../crates/threadlane-store/src/dreaming.rs) · [Desktop memory history](../crates/threadlane-desktop/src/memory.rs)

### Give busy projects stable pages and let quiet projects sleep

Every call to `project_sessions` loads the eligible runs, queries sent messages once per run, loads all project tasks and only then slices the combined list. A newly finished run can move the boundary between runs and tasks while the dream is paging. The choices tool also takes the latest 20 answers before applying the page budget, so a burst of decisions can hide older evidence. The timer already coalesces pending dreams. Letta offers a complementary idea by tying dream work to completed steps or context compaction. [Dream query](../crates/threadlane-store/src/dreaming.rs) · [Tool paging](../crates/threadlane-hub/src/dreaming.rs) · [Timer coalescing](../crates/threadlane-store/src/timer.rs) · [Letta dreaming](https://docs.letta.com/configuration/memory)

Start by freezing eligible record IDs or equivalent stable bounds for one occurrence, then fetch each page and its messages in bounded store queries. Reuse the existing SQL task paging. Preserve full task coverage for limitation review. Persist progress only for work actually reviewed and carry unread pages forward after a restart or budget limit. Once that works, keep the timer as the opportunity to dream and skip model invocation when there is no new evidence, pending work or maintenance due. Allow occasional reference checks even on quiet projects because repository files can move independently. [Existing task paging](../crates/threadlane-store/src/task.rs)

Exercise three cases: a task finishes between pages, more than 20 choices arrive between dreams, and the hub restarts mid-page. Every eligible ID should be accounted for once across the completed batches. On an idle occurrence with no maintenance due, expect zero backend calls. Compare records fetched, backend calls and elapsed time on the same seeded workload before claiming savings.

### Learn one verified project procedure

Noop's Architect looks at failed and blocked tasks. LongMemEval-V2 extends this idea to evaluating whether an agent can recover workflows and recognize recurring failure conditions from experience. Threadlane already gives a dream task IDs, waiting reasons and results. Pilot one ordinary project note with a symptom, applicable conditions, verified fix, source task and pointer to the maintained procedure. A blocked task suggests a question. A successful retry or reviewed result can supply evidence of the answer. Use a project-scoped note and let the next worker find it through the existing notes index. [Noop Architect](https://github.com/huacnlee/noop/blob/199c7ce9de22e0b6d34bad8b805e99e09b2003cf/templates/agents/architect/profile.md) · [Task evidence](../crates/threadlane-hub/src/tools.rs) · [LongMemEval-V2](https://arxiv.org/abs/2605.12493)

For example, seed an export task that failed on an old manifest and a reviewed retry that succeeded after regenerating it. On a fresh matching task, score whether the worker finds the maintained procedure and avoids the failed attempt. On an unrelated export failure, score whether it investigates the new cause. Record the successful task and the number of failed attempts alongside the recall score.

### Distinguish when a rule was learned from when it applies

“Starting with the next release, use the new checklist” has two dates. The version history records when Dreaming wrote the note. The instruction takes effect at a release boundary. Graphiti explicitly distinguishes the time a fact was learned from the period when it held. Threadlane can borrow that distinction by recording an effective date or named milestone in the existing note when the source gives one. If the milestone's status is unknown, the Lead should check it before choosing a checklist. [Graphiti temporal model](https://arxiv.org/html/2501.13956v1#S2.SS2.SSS3) · [Versioned memory](../crates/threadlane-store/src/memory.rs)

Replay decisions immediately before and after the release, plus a historical question about the old release. All three should resolve to the applicable version. This also qualifies the pilot's immediate reversal rule: a correction takes effect now only when the owner means now.

**Delivery decision.** Land the post-dream recall fixture and stale-write check first. Run the owner-correction pilot against those checks, then trial one verified procedure. Prioritize store paging before expanding to a busy project. Enable idle skipping after observing an idle-work baseline. Add effective-date handling when the temporal fixtures expose a miss. Personal session import, a graph database and an always-on second reviewer add cost without answering these first pilot questions.
