# `threadlane-tools`

`threadlane-tools` contains built-in file system tools, pattern searching primitives, and sandboxed process execution for Threadlane.

## Included Primitives

- **File Operations**: `view_file`, `replace_file_content`, `multi_replace_file_content`, and `write_to_file`.
- **Directory & Search**: `list_dir` for directory enumeration and `grep_search` using `ripgrep` for fast pattern matching.
- **Process Execution**: `run_command` with strict working directory containment validation and timeout boundaries.

## Project memory

`manage_memory` keeps short, source-backed findings in the current checkout's
`.threadlane/memory.json`. Native foreground and child agents automatically
receive relevant fresh findings before provider requests. Recall is optional,
budgeted, and request-only: it does not rewrite transcripts or source files.

After inspecting a file with `read_file`, retain its useful conclusion with the
observed SHA-256 from the read header:

```json
{
  "action": "remember",
  "key": "terminal-parser",
  "kind": "fact",
  "content": "Terminal parsing and resize math share the metrics in this module.",
  "sources": [{"path": "crates/threadlane-ui-terminal/src/lib.rs", "sha256": "<observed 64-character SHA-256>"}]
}
```

Use the same key to correct a finding. Identical retries leave its revision and
file unchanged. `kind: "experience"` records a useful verification lesson or
failed approach. Findings require one to four source files and at most 1,000
bytes of content; do not retain raw code, credentials, or unverified guesses.

```json
{"action": "recall", "query": "terminal parser resize"}
{"action": "recall", "include_stale": true}
{"action": "status"}
{"action": "forget", "key": "terminal-parser"}
```

Recall excludes changed, deleted, inaccessible, and oversized source files by
default. Explicit stale inspection marks them `fresh: false`. Fresh only means
the evidence hashes match; findings remain untrusted data, and exact code reads
are still appropriate for edits. Results use deterministic keyword matching,
default to five notes, and fit within 3,000 characters. Long escaped content may
be excerpted while retaining complete source references.
Each recall checks at most 32 ranked candidates and reads at most 16 MiB of
source evidence; exhausted scans return verified results with `omitted_for_budget`.

Storage is versioned, capped at 512 notes / 2 MiB, and committed atomically under
an OS file lock. Full stores reject additions; forget obsolete keys to make room.
Malformed stores are preserved and reported by the tool; automatic recall logs
the error and proceeds without injecting memory. The basic privacy filter
rejects common credential markers, sensitive source paths, and `memory:ignore`;
it is not a complete secret detector. Normal session tool auditing still applies
to tool arguments, so never submit secrets to this tool.

Banks are checkout-scoped. Sessions and child agents in the same checkout share
findings; isolated worktrees and unrelated checkouts do not. Native recall adds
no model calls. ACP agents own their provider requests, so their requests are not
automatically enriched. Read-only policy allows `read`, `recall`, and `status`
and blocks memory mutations.

Legacy `read`, `save`, and `consolidate` actions (and their aliases) still maintain
`.threadlane/memory.md`. That hand-maintained project guidance is preserved;
its updates now use the same serialized, atomic writer discipline.

See [the implementation plan](../../docs/project-memory-plan.md) for the design
study, validation, and measured follow-up gates.
