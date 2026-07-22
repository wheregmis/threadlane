---
description: Workflow preset: scout -> planner -> worker
---

Execute a multi-stage implementation flow:
1. Run `scout` agent to locate relevant files for `$1`.
2. Run `planner` agent using `{previous}` context to build an execution plan.
3. Run `worker` agent to implement the plan.
