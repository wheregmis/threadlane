# Planning before implementation

Use for nontrivial implementation, especially before assigning edits to a subagent or sidekick. The main agent owns this work. Skip formal planning for tiny direct changes; read-only research can precede a plan if the questions and no-edits boundary are explicit.

## Settle the design

1. Read relevant project instructions and inspect the actual code, existing helpers, tests, and caller contracts. Distinguish observed facts from hypotheses.
2. State the desired behavior, acceptance criteria, non-goals, and constraints. For a bug, use the debugging workflow to establish the cause before designing the fix.
3. Resolve consequential choices: ownership, API/interface shape, compatibility, error behavior, and risks. Compare alternatives when they materially change the result. Ask the user only for decisions you cannot responsibly infer; do not send unresolved architecture to a cheaper worker.
4. Share a concise design/plan summary before implementation. Respect requested approval gates; do not invent a mandatory approval pause for every routine edit.

## Write an implementation-ready task plan

A progress checklist is not a task brief. For each bounded task provide:

- Goal, acceptance criteria, and non-goals.
- Evidence: relevant paths/symbols and findings already established; explicitly label assumptions.
- Ownership: files to edit or create, helpers to reuse, interfaces to preserve, and files not to touch.
- Ordered steps with dependencies and settled design decisions. Include signatures, invariants, or pseudocode only where they remove ambiguity; do not transcribe every function body.
- Tests: the few regression cases that prove the change, assertions/expected behavior, exact focused commands, and expected outcomes. Name any broader final gate separately.
- Stop conditions: missing context, violated assumptions, user redirection, or an out-of-scope failure. Define the expected report and any authorized commit/publication boundary.

Self-review the plan against the request: every requirement has an owning task; interfaces agree; dependencies are ordered; the worker can start without redesigning the feature. Avoid speculative scaffolding and plans longer than the change itself.

## Hand off the task, not the planning responsibility

Maintain concise milestones with `update_plan` when available, but put the detailed task plan in the worker's `task`/`instructions`. A plan-file reference is acceptable only if you verified the child can read it in its actual checkout; still include the task's decisions and acceptance criteria inline. Never assume an uncommitted parent file exists in an isolated worktree. Use `context_refs` for supporting code evidence, not as the plan.

Default to one persistent worker and blocking execution. Run independent read-only investigations in parallel only when their outputs are genuinely independent. Do not schedule reviewers before code exists or duplicate the worker's investigation. For follow-ups, reuse the lane and send the changed scope, decisions, and current evidence. The main agent reviews the resulting diff and evidence before accepting completion.
