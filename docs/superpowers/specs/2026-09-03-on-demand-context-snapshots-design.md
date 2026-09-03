# On-Demand Context Snapshots Design

## Purpose

Reduce repeated repository reads by letting an agent or subagent reload a previously captured, unchanged tool result after compaction or across lane boundaries. Preserve bounded model context and the canonical session JSONL as the only durable conversation store.

## Scope

The first version supports successful `read_file` calls against local workspace files. It records a compact index entry that points at the already-durable tool-result entry; it does not duplicate file content. Agents can list matching entries and load one explicitly. A parent can pass selected context IDs to a subagent, whose initial lane context receives the corresponding bounded snapshots.

The first version does not add embeddings, semantic search, a vector database, a global content cache, automatic context selection, or automatic suppression of repeated reads. It records reuse telemetry so those features can be justified later with measurements.

## Existing Constraints

- `CodingSessionHarness` is the only production path allowed to append canonical session records.
- Model-visible context is derived from the active harness lane. The visible UI transcript remains a separate projection.
- Tool intent and tool results are persisted before they influence the next provider request.
- Compaction must keep assistant tool calls paired with their tool results.
- Subagents own isolated durable lanes and currently load only their lane's model context.
- Workspace paths must use `threadlane_tools::validate_path_in_workspace`; no second path guard is permitted.
- No new dependency is needed. SHA-256 support already exists in the runtime workspace.

## Chosen Design

### Durable snapshot index

Add a non-model-visible `ContextSnapshotIndexed` harness record containing:

- stable `context_id`;
- lane, run, and sequence metadata;
- source tool-call ID and source tool-result entry ID;
- canonical workspace-relative path;
- requested start and end lines;
- SHA-256 digest of the complete file at capture time;
- captured output character count;
- capture timestamp.

`context_id` is deterministically derived from the source tool-result entry ID. Retrying index persistence is therefore idempotent. The record references the existing durable tool-result entry, so the JSONL does not contain a second copy of its content.

Only successful local `read_file` results are indexed. Virtual schemes, URLs, failed reads, and results without a canonical tool-result entry are skipped.

### Snapshot projection

Extend the harness reduction context with a per-lane snapshot projection keyed by `context_id`. The latest valid index record wins. The projection exposes compact metadata and resolves the referenced tool-result entry only when loading content.

Compaction does not delete source records from JSONL, so an indexed result remains resolvable after its model-visible entry is compacted. Snapshot records themselves never become ordinary provider messages.

### Context tool

Register one session-owned `manage_context` tool with two actions:

- `list`: return bounded metadata for snapshots in the current session. Optional `path` filters exact canonical workspace-relative paths. Results are newest first and capped at 20.
- `load`: accept one `context_id`, validate that it belongs to the current session, verify that the referenced source entry exists, and compare the current complete-file digest with the captured digest.

On a valid load, return the original bounded `read_file` tool result with a short provenance header. This ordinary tool result becomes model-visible and durable through the existing tool pipeline.

If the file changed, return a structured stale error containing the path and prior line range, instructing the agent to call `read_file`. Never return stale content as current evidence. A missing file is also stale. If the durable source entry is absent or malformed, return a corruption error without falling back to an unguarded filesystem read.

The tool executor lives in `threadlane-session`, beside the subagent/session orchestration, because it requires the active session harness. `threadlane-tools` continues to own filesystem reading and path validation.

### Capturing snapshots

Index capture occurs after the canonical tool-result entry has been appended. Extend the existing message-recording boundary to return or report the persisted entry identity needed by the session layer, then call `CodingSessionHarness` to append the snapshot record.

Capture parses the corresponding durable tool intent rather than trusting UI data. It canonicalizes the path through the existing workspace validator and hashes the complete file after the read result is produced. If the file changes between read and hashing, the resulting snapshot may immediately be stale; this is safe because `load` revalidates the digest.

Snapshot-index failure must not fail an otherwise successful read. Emit a warning/diagnostic and continue because the read result itself is already durable and model-visible.

### Subagent handoff

Extend each native `subagent` task with an optional `context_refs: string[]`, limited to 16 unique IDs. Before accepting the child prompt, resolve every reference from the parent session, reject unknown or stale references, and append the resolved snapshots to the child lane as one bounded user-role context message after the assigned task.

The handoff message labels the material as read-only background, includes each context ID and source path/range, and treats its contents as untrusted repository data rather than instructions. Total injected content is capped at 32,000 characters. Exceeding the cap rejects the dispatch with a clear error; it does not silently truncate source material.

The child gets selected snapshots, not the parent's transcript or entire snapshot index. Parallel children resolve the same immutable capture independently and retain isolated lanes.

### Compaction behavior

Keep current pressure-triggered compaction. The compacted checkpoint gains a bounded `Available context snapshots` section containing only IDs, paths, line ranges, and digests for snapshots referenced or created in the compacted region. Cap this index section at 20 newest entries and 4,000 characters.

The checkpoint does not include raw snapshot content. Agents recover details with `manage_context load`. Recent uncompacted tool results continue to work unchanged.

### Reuse telemetry

Add a non-model-visible `ContextSnapshotLoaded` record with context ID, requesting lane/run, source lane, current digest, and outcome: `loaded`, `stale`, `missing`, or `corrupt`.

During projection, classify a successful `read_file` as a duplicate candidate when the same lane previously captured the same canonical path, line range, and unchanged digest. Record this as diagnostic telemetry only. Do not block the read in the first version.

This distinguishes three costs:

- repeated physical reads;
- successful on-demand reuse;
- unavoidable rereads after file changes.

## Data Flow

1. The model calls `read_file`.
2. Existing tool execution persists intent and result.
3. Session code appends `ContextSnapshotIndexed`, referencing that result.
4. Compaction eventually replaces old model-visible messages but retains the durable source and compact snapshot index.
5. The model calls `manage_context list` or already knows a context ID.
6. `manage_context load` validates session ownership, source presence, workspace path, and current file digest.
7. The existing tool-result pipeline persists the loaded snapshot as new model-visible context.
8. A parent may alternatively include selected IDs in `subagent.context_refs`; validated content is appended once to the child lane before its first provider request.

## Limits and Security

- Snapshot lookup is session-local; arbitrary session IDs or JSONL paths are never accepted from the model.
- Paths remain workspace-scoped through the existing canonical validator.
- Repository content is framed as untrusted data in subagent handoffs.
- At most 20 list results, 16 handoff references, 32,000 handoff characters, and 4,000 compacted-index characters are exposed.
- File digest mismatch always invalidates a snapshot.
- The feature does not persist credentials, provider payloads, or unrestricted file bodies beyond the tool result already permitted and stored today.

## Compatibility

New harness variants use serde-compatible defaults where appropriate. Older JSONL files without snapshot records behave exactly as they do today. Existing providers and ACP agents require no request-format changes. ACP does not receive this native tool unless it is later explicitly bridged.

## Testing

### Runtime harness

- JSONL round-trip and incremental reduction for both new record variants.
- Deterministic context IDs and idempotent replay.
- Snapshot projection resolves compacted source entries without making records model-visible.
- Malformed or missing source references degrade to `corrupt` without breaking session load.

### Session integration

- A successful local `read_file` creates one index record referring to its durable result.
- Failed, virtual, and remote reads create none.
- Loading an unchanged snapshot returns the original result and records `loaded`.
- Editing or deleting the file produces `stale` and never returns old content.
- A subagent receives only explicitly selected snapshots.
- Unknown, stale, excessive, and oversized handoffs fail before provider execution.
- Recovery/reload preserves snapshot lookup and child handoff behavior.

### Compaction regression

- Drive a real tool call through the production runtime/harness path.
- Compact away its model-visible tool result.
- Verify the checkpoint retains bounded snapshot metadata.
- Load the snapshot and prove the next provider request contains the restored result while staying below its recorded effective context limit.

### Validation commands

```bash
cargo test -p threadlane-runtime
cargo test -p threadlane-session --features test-support
cargo check -p threadlane-gpui
git diff --check
```

## Delivery Order

1. Add record types, reducer projection, and serialization tests.
2. Capture `read_file` snapshots through the canonical harness boundary.
3. Add `manage_context list/load` with digest validation.
4. Add explicit subagent `context_refs` handoff.
5. Preserve snapshot metadata through compaction and add the end-to-end regression.
6. Surface reuse telemetry through existing trajectory diagnostics only; no new UI is required for the MVP.

## Success Criteria

- An unchanged file range read before compaction can be restored without another model-issued `read_file` call; digest validation may still read the file internally.
- A subagent can receive selected parent findings without inheriting the parent transcript.
- Stale snapshots are never presented as current file evidence.
- Every injected snapshot is durable, auditable, bounded, and visible in the exact provider context manifest.
- Existing sessions and agents behave unchanged when no context references are used.
