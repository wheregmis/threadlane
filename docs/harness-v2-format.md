# Threadlane Harness V2

This document describes the durable session format used by the V2 harness.

## On-disk format

The existing session file remains the compatibility transcript. V2 writes are
JSON lines in that file as `Entry` values, identified by `lane`, `id`, `seq`,
`parent_id`, `message`, and `terminate`. Harness records are written to the
same session's `<session>.harness.jsonl` sidecar. Both streams share one
monotonic sequence space and are validated when opened.

Legacy session nodes remain readable without an eager rewrite. The first V2
write appends V2 entries and records while preserving the legacy logical tree.
Session metadata and global facts are exposed through the compatibility view;
the V2 `FactSet` records are authoritative after a V2 write.

## Recovery

Opening a session only reduces persisted entries and records. It never starts a
provider, tool, hook, timer, or replay effect. An open lane is reported as
`SuspendedCrash`, or `SuspendedDeferred` when its committed assistant entry
contains a provider handle.

Resume first applies durable deferred writes and reconciles the unfinished
step. Completed tools are skipped. An unfinished tool is replayed only when
both the persisted `ToolStarted` declaration and the current assistant tool
declaration mark it safe and their effective arguments match. Otherwise the
tool result is synthesized as interrupted. Safe replays record physical tool
usage and a new `ToolFinished`; `before_tool` is never rerun after
`ToolStarted`, while `after_tool` runs after an actual replay.

Abort commits `AbortRequested` before reconciliation. It preserves queued
next-run input, writes missing interrupted results and a closing assistant
entry, then commits the terminal operation outcome.

Usage is an append-only ledger: each physical provider request, including a
discarded or failed response, gets a record; each physical tool execution and
safe replay gets its own record; manual corrections use `Adjustment`. Totals
are reduced from these records rather than from a latest-value fact.

Snapshots also carry an optional in-memory streaming projection for the active
assistant response. It is delivered through the event hub, is never persisted,
and is cleared when the response or tool batch reaches its durable boundary.

Compaction is the deliberate context invalidation. Its summary is an appended
root entry, followed by the retained tail; old entries remain available for
navigation but are not traversed by the compacted active branch.

Queue records preserve the queue kind, optional run binding, provisioned target,
and steer priority (`Low`, `Normal`, or `High`). Navigation is an explicit
`OperationStarted` with `Navigation` intent, a `LaneMoved` record, and a
terminal `OperationFinished`; a summary attempt and entry may be inserted
between the move and finish.

## Replay and corruption policy

Every entry and record has a unique non-empty ID and strictly increasing
sequence. Parents, lane names, operation references, tool ordinals, result IDs,
and provisioned queue/deferred entries are validated before append and during
reduction. A malformed complete line, duplicate ID, missing parent, or invalid
sequence faults the store. A torn final JSON line is ignored so a process
crash cannot make the session permanently unreadable.

Storage or invariant failure stops the harness before external work begins.
The UI displays a faulted session as unavailable for resume; operators should
preserve the original files, copy them for diagnosis, and repair or remove
only the torn final line after confirming the preceding records are valid.

## Backend and compatibility rules

MemoryStore defines reducer semantics. JSONL is the supported default backend;
SQLite provides transactional sequence allocation, writer leasing, forks, and
the same reducer validation. Tree-only historical JSONL can still be read as a
transcript, but pre-V2 sidecar operations are not migrated or recovered.

Foreground chat uses the `main` lane. Explicit background `/task` work remains
owned by `HarnessSupervisor`; ordinary foreground sessions and built-in
subagents use the same durable harness core without becoming supervisor tasks.
There is no legacy `.oplog.jsonl` compatibility path. Operation, abort, tool,
queue, navigation, and recovery state are written through V2 records only.
