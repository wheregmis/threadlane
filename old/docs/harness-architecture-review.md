# Harness architecture review

Status: proposal based on source inspection and primary-source browser research. No production behavior is changed by this document. The failure scenarios below are architectural deductions, not claims of reproduced incidents.

## Recommendation

Threadlane's main architectural problem is **split execution ownership**: the durable harness validates and records transitions, while a separate imperative agent loop owns the running continuation. CodingAgent bridges them through callbacks, mutable turn state, and reconciliation. This is not a lack of event sourcing; much of the necessary event-sourced infrastructure already exists. The missing boundary is a single owner of executable session state.

Make one headless session supervisor own foreground runs, child lanes, queued input, approvals, cancellation, and recovery. Persist explicit work intent before dispatch, route completions back to that owner, and make transcript/UI/model context projections of committed state. Preserve the existing Rust providers, tools, permission gates, reducers, and context compiler rather than importing a different agent framework.

An intentionally breaking end state is `threadlane-engine` as a local service, with GPUI, CLI, and ACP as clients. A separate process is optional initially: establish ownership and the command/event protocol in-process first. Moving the current tangle into a daemon would not by itself fix it.


### First implementation slice landed

`crates/threadlane-coding-agent/src/scheduler.rs` now carries a per-session execution lease shared by all scheduler clones. The normal `CodingAgent` scheduled-work loop retains that lease across every batch in one drain, preventing another surface adapter from interleaving provider/tool work between batches. The regression test `scheduler::execution_owner_tests::execution_owner_serializes_session_drivers` proves a second executor blocks until the first releases the lease. This is a serialization boundary, not yet the complete command-mailbox supervisor described below.

## What the current architecture actually does

Approximate foreground path:

```text
CodingAgent accepts input / establishes a durable operation
  -> installs journal/context/tool callbacks on UnifiedAgent
  -> TurnDriver runs an imperative provider/tool loop
       -> provider-boundary callback prepares canonical context
       -> provider trace and message callbacks persist progress
       -> ToolDispatcher records intent, executes, records completion
  -> CodingAgent reconciles history and completes the operation
```

Evidence, with repository-relative paths and line numbers at review time:

- `crates/threadlane-runtime/src/harness/agent.rs:10`: `AgentHarness` owns a store, gated effects, event hub, and hooks.
- `crates/threadlane-runtime/src/harness/effects.rs:8`: `EffectAction` contains `AppendEntry` and `AppendRecord`. It is a journal-effect mechanism, not a provider/tool execution command queue. The name alone should not be read as proof of durable execution.
- `crates/threadlane-coding-agent/src/durable.rs:140`: `install_run_trace_recorders` opens a per-run harness adapter and installs provider tracing, provider-boundary preparation, message recording, tool-intent recording, and tool-completion callbacks onto the live agent.
- `crates/threadlane-runtime/src/turn_driver.rs:249`: the imperative loop passes current messages through the boundary preparer and writes prepared messages back into mutable turn state. At `:824`, that loop dispatches the next tool batch.
- `crates/threadlane-coding-agent/src/durable.rs:947`: `sync_turn_from_model_context` copies the canonical context projection into the live agent. At `:1004`, `sync_harness_and_dispatch_assistant_hooks` compares live and durable histories, including compaction reconciliation.

Having cached or projected state is not inherently wrong. The concern is that the live continuation, durable lane lifecycle, and recovery decisions are implemented in different control paths. Correctness therefore depends on all those paths obeying the same ordering contracts.

### Observable architectural consequence: recovery is asymmetric

- `crates/threadlane-coding-agent/src/runtime.rs:155`: `resume_interrupted_turn` delegates to `recover_interrupted_subagent_lanes`.
- `crates/threadlane-coding-agent/src/durable.rs:1185`: interrupted subagent recovery has a dedicated implementation; around `:1313` it replays claimed safe tools.
- `crates/threadlane-coding-agent/src/runtime.rs:1143-1145`: when no harness run is adopted (or the selected model is ACP), the foreground input path invokes `recover_abort` before accepting new work; this is not universal foreground recovery.
- `crates/threadlane-coding-agent/src/harness/cancel.rs:122`: `recover_abort` finds the open main-lane operation and requests abort if one was not already requested.
- `crates/threadlane-runtime/src/harness/diagnostics.rs:149`: the recovery projection can describe `AbortUnsafeTool`, `ReplaySafeToolsThenResume`, and `ResumeFromLeaf`. A diagnostic classification is not itself a scheduler implementing that action.

Thus the inspected foreground recovery path is conservative interruption/abort recovery, not the same continuation mechanism used for child lanes. This may be intentional product policy; it is not evidence of silent data loss. But the architecture does not currently provide one uniform implementation of resumable execution across lane kinds. If reliable long-running/background agents are the goal, that is the most important boundary to fix.

### Safeguards worth preserving

Do not characterize the current harness as an unprotected logging wrapper:

- Provider-boundary preparation is awaited and failure prevents the request (`turn_driver.rs:249`). Durable coding sessions deliberately avoid the non-durable loop's private compaction path (`turn_driver.rs:214-226`).
- Tool intent recording is awaited before external execution; recording failure returns an error instead (`crates/threadlane-runtime/src/tool_dispatcher.rs:877`).
- Tool completion persistence failures are surfaced (`tool_dispatcher.rs:670`).
- `crates/threadlane-coding-agent/src/harness/replay.rs` has safe-tool claim logic, including a conservative claim record to prevent repeated recovery execution.
- `crates/threadlane-runtime/src/harness/jsonl.rs` already has writer coordination. Multiple short-lived harness handles do not, by themselves, prove concurrent file corruption.
- `crates/threadlane-coding-agent/src/harness/mod.rs:66-119, 182-204` shares per-session event/hook/cancellation infrastructure across adapters. This is useful infrastructure, although not a single execution owner.

The proposed change consolidates these guarantees rather than replacing them with a weaker generic agent loop.

## Research: architectures to borrow from

Primary-source pages opened and read in the browser:

| Source | Documented design | Lesson for Threadlane |
| --- | --- | --- |
| [OpenAI Agents SDK: Running agents](https://openai.github.io/openai-agents-python/running_agents/) | Separates the runner from conversation-memory strategies; documents separate durable-execution integrations with Temporal, Restate, Dapr, and DBOS. | A persisted session does not automatically make execution durable. Define the run contract separately from chat history. |
| [Claude Agent SDK: Work with sessions](https://code.claude.com/docs/en/agent-sdk/sessions) | Sessions persist prompts, tool calls/results, and responses. Continue/resume append to a session; fork copies history. Explicitly states that session persistence does not snapshot the filesystem. | Separate conversational branching from workspace branching and recovery of external effects. Do not promise filesystem rollback when forking history. |
| [LangGraph: Checkpointers](https://docs.langchain.com/oss/python/langgraph/checkpointers) | Checkpoints capture graph state at super-step boundaries. Pending writes preserve successful nodes in a failed step. `sync`, `async`, and `exit` modes make durability tradeoffs explicit. Replay after a checkpoint re-executes subsequent nodes, including external requests. | Persist execution position and completed work, not just messages. Specify the durability boundary and distinguish recovery from intentional re-execution. Checkpointing is not an exactly-once external-effects guarantee. |
| [Codex App Server](https://developers.openai.com/codex/app-server/) (redirects to [current documentation](https://learn.chatgpt.com/docs/app-server)) | Rich clients use thread/turn/item primitives, commands such as start/steer/interrupt, streamed notifications, and server-mediated approvals. | A headless engine/client contract is a good target for Threadlane. Borrow the ownership boundary, not necessarily its provider-specific protocol. |

These are documented architectural comparisons, not benchmarks or claims that these implementations are bug-free. In particular, the Codex client/server boundary does not establish exactly-once execution or complete crash recovery by itself.

## Target design

```text
GPUI / CLI / ACP
       | commands with stable request IDs
       v
SessionSupervisor (one logical writer per session)
       | validate command against committed state
       | commit accepted command + executable work intent
       v
Versioned journal / transactional work store
       |                                  |
       | eligible work                    | committed events
       v                                  v
Provider / Tool / Child workers       UI and context projections
       |
       | completion(effect ID, attempt, owner epoch)
       +-----------------> SessionSupervisor
```

The supervisor serializes state transitions, not slow tool execution. Workers execute concurrently outside the owner; their immutable completions come back through its mailbox. A child lane is scheduled work with a parent relationship, not an unrelated recovery subsystem.

### Required contracts

1. **One lifecycle authority.** Commands such as submit, steer, approve, cancel, resume, and fork are handled by the same owner. Reads may use projections. Neither UI nor workers directly mutate canonical lane lifecycle.
2. **Explicit persisted work.** Give provider requests and tool calls stable effect IDs, attempt numbers, input/context hashes, and states such as pending, running, succeeded, failed, cancelled, and outcome-unknown. Names here are proposed types, not existing API symbols. Record enough payload or immutable artifact references to reconstruct work; a hash alone is insufficient.
3. **Commit before dispatch.** Committed intent must precede external work; committed completion must precede dependent work. The work-store API must define its crash/power-loss guarantees, not merely whether an append returned. Prefer a transactional outbox (SQLite is a reasonable local option), or prove equivalent JSONL recovery semantics. Do not switch formats merely for fashion.
4. **Context is compiled input.** Build an immutable request from a committed context revision plus pinned model, tool schema, prompt, permissions, and compaction metadata. Workers must not independently edit canonical conversation state. Streaming deltas remain provisional until committed final output or an explicitly incomplete checkpoint.
5. **Events are the client contract.** Durable events have monotonic cursors and replayable projections. Token deltas can be transient and coalesced. After event gaps or reconnect, clients request a snapshot plus a cursor. Do not put every token through a synchronous disk transaction.

### Exactly-once is not a valid blanket promise

A journal transaction cannot atomically commit a filesystem mutation, shell command, browser click, remote API call, and its result record.

If a process dies after the side effect but before completion persistence, the honest state is **outcome unknown**. Resolve it by tool contract:

- Pure/read-only work may be retried, with a fresh observation explicitly represented as such.
- An idempotent API can be retried with a stable idempotency key if the adapter guarantees that contract.
- A verifiable mutation may be reconciled against receipts, hashes, or external status.
- An opaque mutation must not be automatically replayed. Require reconciliation or explicit user authorization.

Classify replay safety on tool capabilities/contracts, not just names. Permission must be revalidated at dispatch/retry where required. An approval is bound to effect identity, arguments, and policy context; editing arguments must invalidate it.

## Breaking changes worth making

- Replace recorder-driven continuation ownership with typed session commands and worker completions. Keep transitional callbacks only as adapters while one owner is introduced; do not maintain both architectures indefinitely.
- Replace separate main/subagent recovery algorithms with a shared lane/work state machine and policy differences expressed as data.
- Stop exposing mutable runtime history as an alternative authority. The engine owns canonical events; model requests and GPUI messages are projections with explicit revisions.
- Version the persisted execution schema. Import legacy conversation history faithfully, but mark unprovable in-flight external effects as unknown. Never invent successful completions during migration.
- Once the API is stable, run the supervisor in a local daemon if surviving GPUI restarts is a product requirement. Default to a local authenticated transport; remote operation is a separate security feature. UI-hosted browser/approval capabilities must report disconnection rather than being assumed to survive the UI process.

Keep the existing leaf-level tool executors, provider adapters, permission system, session tree/branch semantics, context provenance, and worktree isolation. Replacing those all at once would increase migration risk without fixing ownership faster.

## Ordered migration and verification

1. **Introduce the session owner in-process.** Route accepted foreground input, cancellation, and scheduling through a command mailbox with stable command IDs. Initially retain current provider/tool adapters behind it. Relevant seams: `crates/threadlane-coding-agent/src/controller.rs`, `scheduler.rs`, `cancellation.rs`, `durable.rs`. Acceptance: duplicate submit has one durable acceptance; a cancelled/stale worker cannot start a subsequent provider step; the same session runs without GPUI.
2. **Make one provider/tool cycle explicit work.** Add persisted work intent/completion through the harness store/procedures, and have the supervisor dispatch it. Migrate `turn_driver.rs` and `tool_dispatcher.rs` one boundary at a time. Acceptance: crash before dispatch, during execution, and after execution/before completion produces the documented recovery state; unsafe work is never blindly replayed. Persist independent parallel completions without waiting for an unrelated slow sibling.
3. **Unify lane recovery.** Move child and main recovery behind the same transition rules, retaining safe-tool claims and conservative unknown-effect handling. Acceptance: foreground and child lanes with identical effect history receive identical recovery treatment unless an explicit policy differs; completed child results are not delivered twice.
4. **Make clients projection-only and optionally extract a daemon.** Route UI/ACP control through the protocol and cursor-based subscription. Acceptance: reconnect reconstructs the same committed transcript/status; UI closure does not cancel daemon-owned work accidentally; missing UI capabilities suspend/fail clearly; cancellation remains available when the active run is awaiting I/O.
5. **Migrate storage and remove compatibility paths.** Use a versioned importer and keep old files recoverable. Acceptance: branches, compaction provenance, queues, and tool-call/result association survive import; ambiguous legacy effects remain visible as unknown; one source of truth remains after deleting transitional reconciliation.

For a runtime implementation, start with `cargo check -p threadlane-runtime -p threadlane-coding-agent`, `cargo test -p threadlane-runtime --lib`, and `cargo test -p threadlane-coding-agent --lib`, then focused crash-boundary tests added for the new implementation. Those commands are proposed implementation checks, not results of this documentation review. Never weaken existing tests or change expected outcomes merely to make validation pass.

Important cases to include in the implementation's acceptance matrix:

- cancellation racing with completion; old owner epochs; duplicated completion and submit delivery;
- partial provider streams, provider retries with different output, and model switches during continuation;
- approval pending across restart, changed arguments, denied/expired permissions, and disconnected UI capabilities;
- compaction versus queued input; forked conversation versus shared/mutated filesystem; child completion after parent cancellation;
- torn journal tails, full disks, schema-version mismatch, and crash after an external effect but before its durable acknowledgement.

## Decision to make first

Choose the intended product contract explicitly: **resumable conversation** or **resumable execution**. The former can legitimately abort interrupted foreground operations, but should say so consistently. For Threadlane's background/subagent trajectory, resumable execution is the stronger target. The first implementation slice should be the in-process session supervisor and one persisted provider/tool cycle, not a full daemon rewrite or another layer of recovery callbacks.
