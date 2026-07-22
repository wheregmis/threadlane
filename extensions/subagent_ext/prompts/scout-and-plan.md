---
description: Workflow preset: scout -> planner
---

Execute a planning flow:
1. Run `scout` agent to explore the codebase for `$1`.
2. Run `planner` agent using `{previous}` context to produce a technical implementation plan.
