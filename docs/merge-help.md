# Task branches that merge cleanly

Dogfooding finding 23: tasks that run in parallel each branch from the
base when they start, and the owner merges their branches by hand later.
When two touched the same request enums, the merge conflicted in four
files. Nothing told anyone until the merge.

## Decisions

1. **The task finds out, not the owner.** Conflicts are cheapest to
   resolve by the agent that wrote the change, while its session still
   holds the context.
2. **Check at finish.** When a task calls `finish_task`, the runner
   fetches the repository's default branch and checks whether the task
   branch merges into it cleanly (`git merge-tree --write-tree`, which
   touches no worktree). If it does, the task finishes as today. If not,
   `finish_task` is refused with the conflicting files and the base
   commit, and the agent is told to rebase onto the base, resolve, run
   the checks again, and finish.
3. **Bring the base in at the start of a run.** Before a task run starts
   on an existing worktree, the runner fetches the default branch. It
   rebases an unpublished task branch when the rebase is clean. A published
   task branch keeps its history so new commits can still be pushed. If the
   check finds conflicts, the run's prompt names them for the agent to resolve.
4. **The result says where it stands.** The task's result records the
   base commit it was last checked against and whether it merged
   cleanly. The board card and task page show "Merges cleanly into
   <branch> at <short sha>" or "Conflicts with <branch>: <files>".
5. **Bounded.** At most two refusals for conflicts per task; on the
   third, `finish_task` succeeds and the result records the conflict, so
   a task never loops.
6. **Only `git`.** No push, no pull request, no network beyond fetching
   the registered remote, which the runner already does.

## Tests

On local bare repositories: a task branch that merges cleanly finishes;
one that conflicts is refused with the files listed, then finishes after
the agent rebases; the third conflict is recorded, not refused; a run
start rebases a clean branch and leaves a conflicting one with the prompt
saying so; the app shows both states.
