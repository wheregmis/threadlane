# Verification and review before completion

Use before claiming completion, accepting a worker's result, or delivering code. Evidence comes before claims.

## Verification gate

1. Re-read the request and agreed plan. Check requirements individually; passing tests alone do not establish scope compliance.
2. Identify the check that supports each claim. Run it against the final relevant code, or inspect a worker's trustworthy evidence for that same revision. Do not rerun unchanged checks merely to duplicate work; rerun when files changed, output is incomplete, or evidence is suspect.
3. Inspect the complete result: command, exit status, failures, skipped tests, and coverage limits. A timed-out or partial suite is not a pass. A lint pass does not prove compilation; a unit test does not prove rendered UI behavior.
4. Review the actual diff against the plan: missing requirements, unrequested changes, caller compatibility, error handling, security, and test quality. Child completion is not parent acceptance.
5. Report only what the evidence supports, and name untested paths and environmental blockers explicitly. Do not claim a UI was visually verified unless it was run and observed.

## Review feedback

Read and assess each finding against the code and intended behavior before acting. Ask for clarification on ambiguous findings; do not blindly implement a suggestion or dismiss it without evidence. Separate required fixes from optional or out-of-scope improvements.

Batch required corrections into one scoped follow-up on the existing worker lane, with exact findings, acceptance criteria, and focused checks. Review the fix diff and new evidence before acceptance; do not expand a scoped re-review into repeated whole-project audits. Escalate recurring failures or a design defect rather than looping indefinitely. Respect publication and merge authorization: completion does not grant permission to merge or delete branches/worktrees.
