# Systematic debugging

Use before changing code for a bug, failing test, or unexpected behavior.

1. Capture the precise symptom: reproduction steps, expected/actual behavior, relevant error output, environment, and affected revision. If it cannot be reproduced, say so and gather discriminating evidence rather than declaring a cause.
2. Trace the executing path from input to failure. Inspect relevant recent changes and compare a working path with the broken one. At component boundaries, check the actual values and ownership transitions; do not log secrets or unrestricted payloads.
3. State a falsifiable hypothesis and the observation that would distinguish it from alternatives. Change one variable or run the smallest diagnostic check. A plausible code smell is not a confirmed root cause.
4. Once supported by evidence, plan the smallest fix at the responsible layer. Add a regression test that fails for the original reason when feasible; verify the failure before the fix and the pass afterward. A broken fixture or compile error is not the intended red test.
5. Run focused checks and relevant integration checks. If a fix fails, reassess the hypothesis rather than piling on unrelated edits. After repeated failed attempts, stop and report the evidence, attempted fixes, and unresolved decision.

When delegating read-only investigation, supply the symptom, questions, files/boundaries, and expected evidence. Do not ask a worker to implement an unverified hypothesis as fact. Record preexisting/environmental failures separately from regressions; never weaken tests or security controls to make a check green.
