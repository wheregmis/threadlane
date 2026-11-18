# Threadlane project memory

Status: initial implementation complete and verified. Scope: a small built-in tool and automatic
recall for native agents, with no server, embedding model, or extra LLM calls.

## Problem and outcome

Agents repeatedly discover the same architecture, verification commands, and
dead ends. Persistent transcripts alone do not solve this: a future session or
child needs the useful finding, its evidence, and a way to detect stale evidence.
File caching saves I/O but still sends file bodies to the model.

The intended outcome is that an agent investigating a known area receives a
short, source-backed finding before choosing tools. Exact file reads remain
available for edits, new questions, and changed code. Memory is background data,
not instructions or proof that a claim is correct.

## Reference study

Reviewed the upstream documentation on 2026-09-30; no upstream implementation
or benchmark was run, and no upstream source code is copied.

| Reference | Keep | Leave out of the initial implementation |
| --- | --- | --- |
| [grit](https://github.com/bofeizhu/grit) | Local storage, provenance, validity checks, bounded deterministic retrieval | Property graph, temporal query language, vector indexes, operation replication |
| [Aver](https://github.com/5queezer/aver/blob/master/doc/how-it-works.md) | Validate before persistence, preserve evidence, retain an audit path, reject common secret material | A second transcript/event log, code extractor, server/OAuth, inference ontology |
| [Hindsight](https://github.com/vectorize-io/hindsight) | Separate retain from recall; isolate memory banks; distinguish findings from experiences; bound retrieved context | LLM extraction/consolidation, reflect service, four-way retrieval, reranker, PostgreSQL |

Hindsight's reflection is a useful future capability. For now the current agent
does the thinking and explicitly retains a concise conclusion. Automatically
promoting arbitrary conversation text into project truth would create a second
problem: false memories that survive longer than their evidence.

## Existing implementation to reuse

- `threadlane-tools::memory`: `manage_memory` and legacy aliases over
  `.threadlane/memory.md`; preserve their formats and behavior.
- `threadlane-prompt`: discovers and injects legacy project memory and advises
  agents to retain useful knowledge.
- Coding harness read snapshots: already capture successful local reads with
  SHA-256, survive compaction, and support `manage_context` and child handoff.
- Harness provider projection: already removes redundant read bodies without
  changing the transcript. Do not create another raw file cache.
- Native `TurnDriver`: the shared provider-request seam for foreground, child,
  revived, and ephemeral native agents. Add optional recall here, before request
  serialization and context-manifest accounting.
- Existing session tool records: retain the normal audit of memory tool calls.
  Do not create another session journal or execution authority.

## Initial delivery plan

### 1. Structured retention

Extend `manage_memory` rather than introduce another model-facing tool:

- `remember`: stable human-readable `key`, concise `content`, `kind` (`fact` or
  `experience`), and one to four source references (`path`, observed `sha256`).
- `recall`: keyword query, bounded result count, optional stale results for
  explicit inspection. Empty query lists recent notes.
- `forget`: remove exactly one key; never touch transcripts or source files.
- `status`: report note count, freshness counts, and capacity.
- Keep `read`, `save`, and `consolidate` for the existing Markdown memory.

Use `.threadlane/memory.json`, a versioned snapshot bounded to 512 notes and
2 MiB. The Markdown file cannot carry machine-validated source provenance, so
structured notes need their own format; this extends the same memory tool and
does not replace hand-maintained project guidance. A same-key update replaces
the current note; exact retries are no-ops. The canonical tool transcript is
the mutation audit, while the snapshot is the current recall state.

All updates use a stable OS-backed lock, reload under that lock, write and sync
a temporary sibling, then atomically replace the snapshot. Apply the same
writer discipline to the legacy Markdown read-modify-write operations. Reject
malformed/unsupported snapshots without replacing them. Never truncate a store
to recover from an error. Missing stores are empty and automatic reads create
no files. Reject symlinked memory storage destinations.

### 2. Evidence and privacy

Require observed source hashes so a note about an old read cannot be silently
attached to a newer file. Validate source containment with the existing path
guard, normalize paths relative to the checkout, reject sensitive path families,
and stream bounded file hashing. Hashes detect changed/deleted evidence; they
do not establish semantic truth or historical authorship.

Reject common credential markers and explicit `memory:ignore` content before
writing. This is a conservative local filter, not a complete secret detector.
Do not ingest raw file bodies, conversations, credentials, or command output
automatically. Render retained text as JSON data with an explicit warning not
to follow embedded instructions.

### 3. Recall and request integration

Rank deterministic keyword matches over note keys, content, and source paths;
prefer key/path matches and break ties deterministically. Revalidate source
hashes for selected candidates; stale notes are excluded by default. Cap output
at 3,000 characters and five default results, keeping complete result records.

Before each native provider attempt, derive a bounded query from the three most
recent real user prompts (or the checkpoint summary if no user prompt remains),
recall off the Tokio worker, and add a request-only background
message only when the complete request remains within its context budget.
Never append retrieved memory to `TurnState`, canonical transcripts, or
compaction summaries. This lets subsequent attempts see new/changed notes
without accumulating duplicate memory. Give the added manifest item a memory
label so its token cost can be inspected using existing telemetry.

Agents retain findings after useful exploration and before delegation or
completion. Child agents operating in the same checkout see the same store
through the shared native request seam. Narrow child tool whitelists remain
authoritative; recall injection does not grant mutation tools.

### 4. Compatibility and verification

Update tool schema, prompt guidance, tool documentation, and repository rules.
Verify through the real tool dispatcher and native runtime/provider path:

- Remember, reload, recall, update, forget, exact retry, and Unicode handling.
- Changed/deleted sources are absent by default and explicitly stale on request.
- Wrong observed digests, traversal, escaping symlinks, secrets, invalid fields,
  oversized files/stores, and unsupported schemas fail without losing data.
- Concurrent writers retain distinct notes; failed persistence preserves the
  previous valid snapshot; missing-store recall leaves the checkout untouched.
- Runtime requests receive memory, subsequent attempts refresh it, persisted
  transcripts exclude injected memory, and manifests include its token cost.
- A new session and a child-shaped native runtime in the same checkout reuse
  findings; an unrelated checkout receives none. Memory fits the effective
  context limit and does not cause optional recall to fail an otherwise valid
  request.
- Existing tools, read reduction, and compaction tests continue to pass.

Run focused Nextest tests, `cargo check -p threadlane-gpui`, and
`git diff --check`; broaden native engine coverage after focused checks pass.
This delivery changes no UI, so do not claim visual verification.

## Deliberate limits and next phases

1. **Measure before expanding.** Use existing efficiency reports to compare
   repeated snapshot reads, request tokens, and completion evidence on the same
   tasks with/without retained findings. Deterministic integration tests prove
   delivery and freshness, not that a particular model will obey the memory.
2. **Worktree sharing.** Initial banks are checkout-scoped, consistent with
   current `.threadlane/memory.md` and path guards. Isolated worktrees keep their
   own banks. A future project-owned bank must use explicit owning-project
   identity, validate evidence against the consuming checkout, and add a
   memory-only capability instead of widening ordinary filesystem permissions.
   Do not infer bank identity from a path-shaped Git common directory.
3. **Automatic retention.** Add candidate observations derived from canonical
   tool/result identities, with explicit promotion and evidence coverage, only
   after measuring missed discoveries. Reuse read snapshots and harness records;
   never make an unverified summary overwrite project facts.
4. **Retrieval scale.** Move the bounded scan to SQLite FTS5 if the 512-note
   ceiling or measured latency warrants it. Add embeddings only if a realistic
   recall evaluation shows keyword misses. Preserve the same tool contract.
5. **Reflection and contradiction handling.** Let agents inspect related facts,
   propose replacements, and retain provenance. Add a paid/background reflect
   workflow only with a concrete cost budget, explicit provenance, and a tested
   approval/promotion policy. No automatic confidence scores posing as truth.
6. **Review surface and external agents.** Add a native browser/editor for
   memories if users need it. ACP agents currently own their provider requests;
   they may use the existing workspace tools but native automatic injection does
   not intercept their model calls. Integrate through ACP's supported context
   contract rather than duplicating its runtime.

## Completion record

Implemented the existing `manage_memory` tool's `remember`, `recall`, `forget`,
and `status` actions, with versioned source-backed notes, freshness checks,
bounded output, atomic writes, and concurrent-writer locking. Legacy markdown
memory actions remain supported. Native provider requests automatically receive
fresh relevant notes within their context budget; canonical transcripts remain
unchanged and context manifests expose the recall's token cost. Read-only policy
permits recall/status and blocks memory mutations.

Validation completed:

- `cargo check -p threadlane-gpui`: passed, with 17 existing warnings.
- Nextest across `threadlane-tools`, `threadlane-runtime`,
  `threadlane-coding-agent`, `threadlane-compaction`, and `threadlane-prompt`:
  369 passed, four skipped (two ignored and two excluded baseline failures).
- The excluded provider tests, `tool_images_translate_to_provider_parts` and
  `tool_without_images_keeps_legacy_shapes`, failed at the same unchanged lines
  with this implementation temporarily removed. They are pre-existing orphan
  tool-result fixture failures, unrelated to project memory.
- `git diff --check`: passed.

Integration coverage exercises actual tool dispatch, durable sessions, a
dedicated child lane, later sessions, checkpointed context, stale evidence,
context ceilings, and manifests. Store tests exercise concurrent threads and
processes, source/storage path guards, corruption, capacity, and atomic legacy
writes. These checks do not establish live-model read savings or UI behavior.
Retention remains explicit, retrieval uses keywords, banks remain checkout-scoped,
and ACP provider requests do not receive native automatic injection.
