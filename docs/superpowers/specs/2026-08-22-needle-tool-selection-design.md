# Needle Tool Selection Design

## Summary

Threadlane will use Needle 2 as a local retrieval stage that reduces a large tool catalogue to five candidates before the first provider request for a user turn. The provider remains authoritative: Needle never executes tools or supplies arguments. Any unavailable, ambiguous, or unsupported routing state falls back to the complete tool catalogue.

Historical Threadlane sessions will be used first as a read-only evaluation corpus, not as training data. The feature may be considered validated when at least 200 eligible historical turns achieve at least 99% top-five recall. LoRA fine-tuning and model distribution are outside this phase.

## Current State

`crates/threadlane-runtime/src/local_tool_router.rs` already loads a Needle 2 `.cact` model behind the optional `threadlane-runtime/needle` feature. It currently asks Needle to generate a single function call, extracts one generated tool name, and filters the provider catalogue to that tool. Failures return the complete catalogue.

`crates/threadlane-runtime/src/turn_driver.rs` calls this router before every provider attempt using the most recent user message as its query. That is unsafe for continuation attempts: a later tool call can depend on a prior tool result rather than on the original user message.

The desktop crate does not enable `threadlane-runtime/needle`, so the existing settings switch cannot currently activate the compiled integration. Its copy also describes file indexing, although the implemented feature is tool routing. The local model at `needle/needle2.cact` is intentionally ignored by Git; `THREADLANE_NEEDLE_WEIGHTS` can override that path.

Canonical session JSONL already records user entries, assistant tool calls, tool execution outcomes, run/lane identity, and enabled tool names. It deliberately does not persist unrestricted tool schemas, so offline evaluation must receive the current catalogue separately rather than reconstructing schemas from session files.

## Goals

- Preserve every successfully executed first-turn historical tool in a five-tool shortlist for at least 99% of eligible turns.
- Reduce tool-schema tokens sent to the provider on first attempts with more than five available tools.
- Keep tool selection local and preserve the full catalogue on every failure path.
- Produce a reproducible, aggregate-only history evaluation without modifying session files.
- Make the desktop setting functional and accurately describe its behavior.

## Non-Goals

- Fine-tuning, pretraining, synthetic-data generation, or online learning.
- Selecting tool arguments or executing tools with Needle.
- Routing provider continuation attempts after any assistant/tool activity.
- Bundling, downloading, updating, or licensing the 14 MB model artifact.
- Persisting prompts, arguments, rankings, or a new analytics database.
- File, symbol, or semantic code indexing.

## Runtime Design

### Eligibility

Needle retrieval runs only when all of the following are true:

1. Needle routing is enabled.
2. More than five tools are configured.
3. The provider attempt is the first attempt immediately following a user or user-with-images message.
4. A Needle model can be loaded.
5. No other Needle inference is running.

Continuation attempts, including attempts after tool results, receive the full catalogue. This deliberately gives up some token savings to avoid excluding tools required by chained workflows.

### Retrieval input

The query is the current user message text. Images are not passed to Needle; image-bearing requests use their text component.

Each candidate is rendered deterministically from the existing `AgentToolDefinition`: tool name, description, and compact parameter schema. The same renderer is shared by runtime routing and offline evaluation so the benchmark measures the production input shape.

The runtime calls Needle 2's contrastive retrieval head with `top_k = 5`. Returned indexes are validated against the current definition slice, deduplicated, and mapped back to complete provider tool definitions. The shortlist preserves retrieval rank.

### Fail-open behavior

The complete tool catalogue is returned when:

- routing is disabled;
- five or fewer tools exist;
- the request is a continuation attempt;
- the model file is missing or cannot be loaded;
- the model lacks a retrieval head;
- retrieval returns no valid indexes;
- another inference is already active;
- inference fails or exceeds the existing two-second timeout.

No partial shortlist is used after an error. Logs may contain the routing outcome, selected tool names, count, and duration, but never prompts or arguments.

### Model availability

The desktop build enables the existing `threadlane-runtime/needle` feature. The settings surface is renamed from “Local Needle Indexing” to “Local Needle Tool Routing” and describes local top-five shortlisting.

Before accepting the enabled state, Threadlane checks that the configured model path exists and that Needle can load it. An unavailable model produces a concise settings error and leaves routing disabled. The existing environment override and ignored repository-local default remain unchanged.

## Historical Evaluation

### Command contract

A developer command accepts explicit inputs:

```text
needle-history-eval --sessions <directory> --tools <provider-tools.json>
```

`--sessions` contains canonical Threadlane session JSONL files. `--tools` contains the current provider-format tool definitions used for both candidate rendering and the catalogue fingerprint. The model path continues to come from `THREADLANE_NEEDLE_WEIGHTS` or the existing default.

The command is local-only, does not make network calls, does not modify either input, and prints aggregate results. Malformed files are counted and skipped without aborting other sessions. An unreadable input directory, unreadable catalogue, invalid catalogue JSON, or unloadable model is a command error.

### Example extraction

For each canonical session lane, the evaluator:

1. Identifies an accepted user entry.
2. Finds the first assistant response belonging to that user turn.
3. Reads tool calls from only that first assistant response.
4. Keeps calls whose matching execution outcome succeeded.
5. Deduplicates retries by prompt entry ID and tool name.

A turn is eligible when it has at least one successful first-response tool call, contains no more than five distinct successful tool names, and every expected name exists in the supplied current catalogue. Failed, cancelled, declined, continuation-only, text-only, obsolete-tool, malformed, and over-five-label turns are skipped with separate counts.

A turn passes top-k recall only when every expected tool name appears in the first `k` retrieved candidates. This strict per-turn definition prevents a multi-tool turn from passing when only one of its tools was retained.

### Report

The report contains:

- eligible and total skipped turns;
- skipped counts by reason;
- top-one, top-three, and top-five passing turns and recall percentages;
- p50 and p95 retrieval latency;
- misses grouped by expected tool name;
- Needle model fingerprint;
- supplied catalogue fingerprint.

It never prints prompt text, tool arguments, tool results, session identifiers, filesystem paths from sessions, or raw JSONL records.

Fingerprints are lowercase SHA-256 hex digests: the model digest covers the `.cact` bytes and the catalogue digest covers the compact serialized tool array. Latency percentiles use the nearest-rank value from the sorted per-turn durations, making reports deterministic without another statistics dependency.

### Decision gate

The result is:

- `pass` when at least 200 turns are eligible and top-five recall is at least 99%;
- `fail` when at least 200 turns are eligible and top-five recall is below 99%;
- `inconclusive` when fewer than 200 turns are eligible.

Exit statuses are `0` for `pass`, `1` for `fail`, `2` for `inconclusive`, and `3` for a command/input error. Automation therefore cannot confuse insufficient evidence with validation.

Passing the benchmark does not automatically change user settings. Needle routing remains an explicit opt-in and retains all runtime fallbacks.

## Testing

One focused test layer covers each boundary:

- Pure unit tests validate ranked-index mapping, deduplication, invalid indexes, top-five truncation, and complete-catalogue fallback.
- History-extraction fixtures validate successful first-turn calls and exclude failures, cancellations, retries, continuations, obsolete tools, and turns with more than five labels.
- Metric tests validate strict multi-tool recall, percentile calculation, and the 200-example/99% decision boundary.
- An ignored real-model test loads `needle2.cact`, retrieves from a synthetic catalogue, and asserts that a relevant registered tool appears in the top five.

Required validation commands are:

```bash
cargo test -p threadlane-runtime --features needle
cargo check -p threadlane-gpui
git diff --check
```

UI behavior is not described as visually verified unless the desktop application is run and observed.

## Rollout

1. Implement the evaluator and retrieval-only runtime path behind the existing opt-in.
2. Run the evaluator against local session history and record only its aggregate report outside the repository.
3. If the gate passes, use Needle locally for first-turn shortlisting while monitoring fail-open logs.
4. If the result is inconclusive, accumulate more eligible sessions without changing routing policy.
5. If the gate fails, inspect aggregate misses by tool name and improve tool descriptions before considering LoRA.

Model bundling or download support is a separate release design initiated only after the local gate passes and the distribution/license requirements are known.

## References

- [Needle 2 repository](https://github.com/cactus-compute/needle)
- [Needle API and retrieval behavior](https://github.com/cactus-compute/needle/blob/main/doc/apis.md#tool-retrieval)
- [Needle fine-tuning format and limitations](https://github.com/cactus-compute/needle/blob/main/doc/finetuning.md)
