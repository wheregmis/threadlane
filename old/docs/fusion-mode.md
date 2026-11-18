# Fusion: persistent lead and worker execution

Fusion keeps the selected main model in charge of planning, ambiguity, and
acceptance. The configured Fusion model executes bounded work in child lanes.
The main model does not get downgraded after a successful delegation. Using the
same model for both roles is supported, but does not imply a price advantage.

## Efficient delegation

Before assigning implementation, write an implementation-ready task plan, not
just progress milestones. Normal delegation and Fusion share the same contract;
see [built-in agent workflows](agent-workflows.md) for the task-brief fields,
on-demand workflow skills, and worker reporting rules.

- Keep tiny changes and context-dependent serial debugging on the lead. Hand off
  implementation, focused verification, and repairs as one bounded job when that
  avoids more work than coordination adds.
- Brief the worker with the objective, settled findings, constraints, owned files,
  acceptance checks, and stopping condition. Resolve consequential design
  decisions before implementation. Use existing `context_refs` for relevant
  source evidence rather than copying whole files or the parent transcript.
- Prefer one persistent worker. Blocking execution is appropriate when the lead
  has nothing independent to do. Use `wait=false` only for genuine overlap,
  `hub wait` for completion, and `hub send` to steer a running lane.
- Use `hub revive` for related follow-ups. Send changed assumptions and feedback,
  not the entire original brief. Review the returned diff and verification
  evidence before accepting it. Batch substantive review findings together.
- Parallelize independent scopes only. Existing parallel write-capable lanes use
  isolated worktrees and require a clean parent checkout. They are not eligible
  for shared-workspace revival; start a new child when the workspace, isolation,
  or configured model is incompatible.

These are model instructions, not a guarantee that a particular model will
delegate optimally or obey the acceptance contract. Tool policies and durable
lane checks enforce the boundaries that cannot safely depend on instructions.

## Context and recovery guarantees

The main Fusion system directive is independent of the current task. Keyword
classification remains audit metadata, not a changing system-prompt suffix or an
automatic routing command. This removes one avoidable source of prompt-prefix
invalidation; it does not guarantee provider cache hits, since providers, tools,
other prompt content, and cache lifetimes also affect caching.

New child lanes capture their effective system instructions and tool allowlist in
the canonical session journal. Revival and interrupted recovery reuse this
contract rather than silently picking up a changed agent definition or dropping
dynamic tool restrictions. Model, workspace, and isolation compatibility checks
still apply. No provider credentials belong in this snapshot.

Prompt capture respects `THREADLANE_REDACT_SYSTEM_PROMPTS` and the existing prompt
size limit. Redacted metadata is persisted without the raw prompt, and the first
execution continues with the already-held in-memory instructions. A saved
contract with an unavailable/redacted prompt, invalid data, or a prompt hash
mismatch cannot be restored: start a new child with an explicit brief. Explicit
Fusion revival also rejects older lanes without a saved contract; legacy
interrupted recovery retains its compatibility path.

Worker outcome monitoring consumes events while the child runs, keeping bounded
state rather than retaining its entire event stream. A successful tool result
resets the consecutive tool-error streak, so a repaired single failure does not
automatically request escalation. Repeated terminal tool errors, questions, and
permission signals still surface to the lead. Lost event evidence must not be
reported as a clean completion. A completed child is not automatically an
accepted solution: lead review remains required.

## Research rationale

The design draws on these primary sources:

1. [Cognition: Introducing Fusion in Devin Desktop & CLI](https://cognition.com/blog/local-fusion)
   describes persistent contexts, exchanging briefs/results instead of whole
   conversations, and retaining frontier-model review. It also cautions that the
   right briefing and exploration policy depends on the model pair.
2. [Cognition: Making Fable Cheaper Than Opus](https://cognition.ai/blog/making-fable-cheaper-than-opus)
   describes avoidable lead turns, repeated exploration, and fragmented handoffs.
   Its reported measurements are for Cognition's harness and model pairs, not
   Threadlane. They motivate reducing duplicate work, not a Threadlane savings
   claim or a hard-coded preference for a named model.
3. [Anthropic: How we built our multi-agent research system](https://www.anthropic.com/engineering/multi-agent-research-system)
   emphasizes clear objectives, boundaries, output contracts, and scaling effort
   to task complexity. Research-task parallelism is not evidence that arbitrary
   coding changes should be split into concurrent writers.
4. [Cognition: Don't Build Multi-Agents](https://cognition.ai/blog/dont-build-multi-agents)
   explains context loss and conflicting implicit decisions. Its caution informs
   explicit ownership and preserved worker contracts rather than unrestricted
   fan-out.

## Validation and limits

Deterministic regression tests cover system-directive stability, saved worker
contracts, revival/recovery boundaries, and outcome/escalation handling without
requiring provider credentials. They establish harness behavior, not improved
model intelligence, end-to-end task quality, latency, or spend.

Before claiming a quantitative improvement, compare the same representative
tasks and model pairs with and without these changes. Include tiny edits, serial
debugging, bounded multi-file implementation, failed checks, follow-up review,
and restart/compaction. Count lead **and** child input/output tokens, reported
cached tokens, handoff/rework counts, wall time, and accepted task outcomes.
Keep unreported prices and cache usage unknown; do not derive savings from model
labels, keyword confidence, or a successful worker exit alone.
