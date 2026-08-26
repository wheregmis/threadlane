# Needle Tool Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Use Needle 2's local retrieval head to shortlist five tools on first provider attempts and validate it against successful historical tool calls before relying on it.

**Architecture:** Keep routing in `threadlane-runtime`: render each existing tool definition once, ask Needle's contrastive head for five indexes, and fail open to the complete catalogue. Add a read-only evaluator that joins canonical session entries and tool outcomes without persisting prompts, then expose it through a stdlib-only developer CLI. The GPUI setting enables the existing optional feature only after the configured model loads.

**Tech Stack:** Rust 1.95+, Tokio, serde/serde_json, sha2, existing `needle-infer`, existing Harness V2 JSONL types, GPUI.

**Spec:** `docs/superpowers/specs/2026-08-22-needle-tool-selection-design.md`

## Global Constraints

- Needle only shortlists; the provider remains authoritative for calls and arguments.
- Route only the first provider attempt immediately following a user message.
- Use `top_k = 5`; five or fewer configured tools always bypass Needle.
- Missing model, missing retrieval head, contention, invalid rankings, errors, and the two-second timeout all return the complete catalogue.
- Evaluation is local and read-only; output never includes prompts, arguments, results, session IDs, or session paths.
- Validation requires at least 200 eligible turns and at least 99% strict per-turn top-five recall.
- Do not add dependencies or distribute the ignored `.cact` model.
- Preserve unrelated worktree changes, including `crates/threadlane-gpui/src/screens/right_panel/view.rs`.

---

## File Structure

- Modify `crates/threadlane-runtime/src/local_tool_router.rs`: model loading, candidate rendering, ranked-index mapping, fail-open retrieval.
- Modify `crates/threadlane-runtime/src/turn_driver.rs`: first-attempt eligibility based on the last model-visible message.
- Create `crates/threadlane-runtime/src/needle_history_eval.rs`: read-only session extraction, metrics, fingerprints, report, decision statuses.
- Modify `crates/threadlane-runtime/src/lib.rs`: export the evaluator module behind `needle`.
- Create `crates/threadlane-runtime/src/bin/needle_history_eval.rs`: stdlib argument parsing and exit-code mapping.
- Modify `crates/threadlane-runtime/Cargo.toml`: declare the feature-gated evaluator binary.
- Modify `crates/threadlane-gpui/Cargo.toml`: enable `threadlane-runtime/needle` for the desktop build.
- Modify `crates/threadlane-gpui/src/services/settings.rs`: reject enabling when the model cannot load.
- Modify `crates/threadlane-gpui/src/screens/settings/view.rs`: accurate tool-routing copy and unavailable error display through the existing status path.
- Modify `README.md`: document local weights, evaluation command, input shape, and exit codes.
- Modify `AGENTS.md`: record the durable first-attempt/fail-open Needle routing convention.

---

### Task 1: Replace generated calls with ranked retrieval

**Files:**
- Modify: `crates/threadlane-runtime/src/local_tool_router.rs`
- Modify: `crates/threadlane-runtime/src/lib.rs`

**Interfaces:**
- Produces: `pub const NEEDLE_TOP_K: usize = 5`
- Produces: `pub fn render_needle_candidate(&AgentToolDefinition) -> String`
- Produces: `pub fn validate_needle_model() -> Result<(), String>`
- Produces under `needle`: `pub(crate) fn needle_engine() -> Result<Arc<V2Engine>, String>` and `pub(crate) fn needle_model_path() -> PathBuf` for the evaluator.
- Preserves: `shortlist_from_environment(query, definitions, enabled) -> Vec<AgentToolDefinition>`

- [ ] **Step 1: Replace the name-set tests with ranked-index and rendering tests**

Add tests that pin rank order, deduplication, invalid-index fallback, compact schema rendering, and the five-or-fewer bypass:

```rust
#[test]
fn maps_ranked_indexes_in_retrieval_order() {
    let definitions = vec![tool("alpha"), tool("beta"), tool("gamma")];
    let ranked = vec![(2, 0.9), (0, 0.8), (2, 0.7)];
    assert_eq!(
        definitions_for_ranks(&ranked, &definitions, 5),
        Some(vec![tool("gamma"), tool("alpha")])
    );
}

#[test]
fn rejects_any_out_of_range_rank() {
    let definitions = vec![tool("alpha"), tool("beta")];
    assert_eq!(definitions_for_ranks(&[(0, 1.0), (9, 0.5)], &definitions, 5), None);
}

#[test]
fn renders_name_description_and_compact_parameters() {
    let rendered = render_needle_candidate(&AgentToolDefinition::new(
        "search_code",
        "Search workspace code.",
        json!({"type": "object", "properties": {"query": {"type": "string"}}}),
    ));
    assert_eq!(
        rendered,
        "search_code\nSearch workspace code.\n{\"properties\":{\"query\":{\"type\":\"string\"}},\"type\":\"object\"}"
    );
}
```

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```bash
cargo test -p threadlane-runtime local_tool_router::tests
```

Expected: compilation fails because `definitions_for_ranks` and `render_needle_candidate` do not exist.

- [ ] **Step 3: Implement deterministic candidate rendering and ranked mapping**

Use existing serde support and no new abstraction:

```rust
pub const NEEDLE_TOP_K: usize = 5;

pub fn render_needle_candidate(definition: &AgentToolDefinition) -> String {
    format!(
        "{}\n{}\n{}",
        definition.name,
        definition.description.as_deref().unwrap_or_default(),
        serde_json::to_string(&definition.parameters).unwrap_or_else(|_| "null".into())
    )
}

fn definitions_for_ranks(
    ranked: &[(usize, f32)],
    definitions: &[AgentToolDefinition],
    max_tools: usize,
) -> Option<Vec<AgentToolDefinition>> {
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for (index, _) in ranked {
        let definition = definitions.get(*index)?;
        if seen.insert(*index) {
            selected.push(definition.clone());
        }
        if selected.len() == max_tools {
            break;
        }
    }
    (!selected.is_empty()).then_some(selected)
}
```

Delete the now-unused name-based `LocalToolRouter` wrapper and `filter_tool_definitions`; no workspace caller uses them. Remove its re-export from `crates/threadlane-runtime/src/lib.rs`. The private ranked helper is the single selection path.

- [ ] **Step 4: Refactor model loading into one cached accessor and add availability validation**

Move the engine `OnceLock` to module scope under `#[cfg(feature = "needle")]`. Resolve `THREADLANE_NEEDLE_WEIGHTS` in `needle_model_path()` before the ignored repository-local default, check `is_file()` before caching a load failure, and return a concise error that contains no provider/session data. Return a cloned `Arc<V2Engine>` from the crate-private `needle_engine()` so the evaluator reuses the same loaded model.

```rust
#[cfg(feature = "needle")]
pub fn validate_needle_model() -> Result<(), String> {
    needle_engine().map(|_| ())
}

#[cfg(not(feature = "needle"))]
pub fn validate_needle_model() -> Result<(), String> {
    Err("Needle support is not compiled into this build.".into())
}
```

Keep the successfully loaded `Arc<V2Engine>` cached so settings validation and routing do not load 14 MB twice.

- [ ] **Step 5: Replace `generate` with `retrieve_tools` and retain every fallback**

Inside the existing `spawn_blocking` closure:

```rust
let rendered = definitions
    .iter()
    .map(render_needle_candidate)
    .collect::<Vec<_>>();
let descriptions = rendered.iter().map(String::as_str).collect::<Vec<_>>();
let ranked = engine.retrieve_tools(&query, &descriptions, NEEDLE_TOP_K);
definitions_for_ranks(&ranked, &definitions, NEEDLE_TOP_K)
    .unwrap_or_else(|| definitions.clone())
```

Return early with the complete catalogue when `definitions.len() <= NEEDLE_TOP_K`. Keep the existing `IN_FLIGHT` gate, `spawn_blocking`, two-second timeout, and full-catalogue branches. Log only selected names/count/duration.

- [ ] **Step 6: Update the ignored real-model test**

Rename it to `needle_v2_retrieves_weather_in_top_five`, provide at least six synthetic definitions, call `retrieve_tools`, map indexes through `definitions_for_ranks`, and assert `get_weather` is present. Do not assert exact score or exact rank.

- [ ] **Step 7: Run focused tests**

Run:

```bash
cargo test -p threadlane-runtime --features needle local_tool_router::tests
```

Expected: all non-ignored tests pass; the real-model test remains ignored.

- [ ] **Step 8: Commit the retrieval core**

```bash
git add crates/threadlane-runtime/src/local_tool_router.rs crates/threadlane-runtime/src/lib.rs
git commit -m "feat(runtime): route tools with Needle retrieval"
```

---

### Task 2: Limit routing to the first provider attempt

**Files:**
- Modify: `crates/threadlane-runtime/src/turn_driver.rs`

**Interfaces:**
- Consumes: `shortlist_from_environment(query, definitions, enabled)` from Task 1.
- Produces: private `needle_query(u32, &[AgentMessage]) -> Option<&str>`.

- [ ] **Step 1: Add eligibility tests at the bottom of `turn_driver.rs`**

```rust
#[cfg(test)]
mod needle_tests {
    use super::*;

    #[test]
    fn routes_only_when_the_last_message_is_user_text() {
        let user = vec![AgentMessage::User { content: "search code".into() }];
        assert_eq!(needle_query(1, &user), Some("search code"));
        assert_eq!(needle_query(2, &user), None);

        let continued = vec![
            AgentMessage::User { content: "search code".into() },
            AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "search".into(),
                content: "result".into(),
                is_error: false,
                terminate: false,
            },
        ];
        assert_eq!(needle_query(1, &continued), None);
    }
}
```

Also assert that `UserWithImages` returns its text and that `Assistant`/`Custom` tails return `None`.

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p threadlane-runtime turn_driver::needle_tests
```

Expected: compilation fails because `needle_query` does not exist.

- [ ] **Step 3: Implement the last-message check and use it in the provider loop**

```rust
fn needle_query(turn_number: u32, messages: &[AgentMessage]) -> Option<&str> {
    if turn_number != 1 {
        return None;
    }
    match messages.last()? {
        AgentMessage::User { content }
        | AgentMessage::UserWithImages { content, .. } => Some(content),
        _ => None,
    }
}
```

Replace the reverse scan for any historical user message. Pass `turn_number as u32`; if `needle_query` returns `None`, clone `configured_tool_definitions` directly, otherwise call the async router. This guarantees retries and tool-result continuations receive the full catalogue.

- [ ] **Step 4: Run the focused runtime tests**

```bash
cargo test -p threadlane-runtime turn_driver::needle_tests
cargo test -p threadlane-runtime --features needle
```

Expected: both commands pass, excluding ignored tests.

- [ ] **Step 5: Commit first-attempt routing**

```bash
git add crates/threadlane-runtime/src/turn_driver.rs
git commit -m "fix(runtime): route Needle only on first attempts"
```

---

### Task 3: Build the read-only history evaluator

**Files:**
- Create: `crates/threadlane-runtime/src/needle_history_eval.rs`
- Modify: `crates/threadlane-runtime/src/lib.rs`

**Interfaces:**
- Consumes: `JsonlStore::open_read_only`, `SessionStore::transcript`, `Record::ToolExecutionObserved`, `render_needle_candidate`, and the cached Needle engine.
- Produces: `NeedleEvalConfig { sessions_dir: PathBuf, tools_path: PathBuf }`.
- Produces: `NeedleEvalDecision::{Pass, Fail, Inconclusive}` and public `exit_code() -> i32` with values `0`, `1`, `2`.
- Produces: `NeedleEvalReport` and `run_needle_history_eval(&NeedleEvalConfig) -> Result<NeedleEvalReport, String>`.

- [ ] **Step 1: Add extraction tests using `MemoryStore`**

Create helpers that append a user entry, an assistant `RuntimeToolCall`, and a matching tool result. Pin these cases in one compact test fixture:

```rust
#[test]
fn extracts_only_successful_first_assistant_tools() {
    let mut store = MemoryStore::new("session");
    let user = store.append_message(None, AgentMessage::User { content: "find rust files".into() });
    let assistant = store.append_message(
        Some(user),
        assistant_call("call-1", "search_files"),
    );
    store.append_message(
        Some(assistant),
        AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "search_files".into(),
            content: "src/lib.rs".into(),
            is_error: false,
            terminate: false,
        },
    );

    let catalog = HashSet::from(["search_files".to_string()]);
    let extracted = extract_store_examples(&store, &catalog);
    assert_eq!(extracted.examples.len(), 1);
    assert_eq!(extracted.examples[0].expected, BTreeSet::from(["search_files".into()]));
}
```

Extend the fixture to assert that a failed tool result is excluded, a later continuation-only call is counted separately, repeated records for the same prompt/tool do not duplicate labels, obsolete tools are skipped, and more than five successful labels are skipped.

- [ ] **Step 2: Add metric and decision tests**

```rust
#[test]
fn strict_recall_requires_every_expected_tool() {
    let expected = BTreeSet::from(["read_file".into(), "search".into()]);
    assert!(!turn_passes(&expected, &["read_file".into()]));
    assert!(turn_passes(&expected, &["read_file".into(), "search".into()]));
}

#[test]
fn decision_requires_two_hundred_examples_and_ninety_nine_percent() {
    assert_eq!(decision(199, 199), NeedleEvalDecision::Inconclusive);
    assert_eq!(decision(200, 198), NeedleEvalDecision::Pass);
    assert_eq!(decision(200, 197), NeedleEvalDecision::Fail);
}
```

Add a nearest-rank percentile assertion over `[1, 2, 3, 4, 100]` so p50 is `3` and p95 is `100`.

- [ ] **Step 3: Run evaluator tests and verify they fail**

Run:

```bash
cargo test -p threadlane-runtime --features needle needle_history_eval::tests
```

Expected: compilation fails because the evaluator module and helpers do not exist.

- [ ] **Step 4: Implement session discovery and extraction**

Discover only direct `*.jsonl` children of `sessions_dir`, skip `*.harness.jsonl`, and sort paths. Open each with `JsonlStore::open_read_only`; count malformed files without printing their paths.

For every lane in `store.entries()`:

1. Use `store.transcript(lane)` for sequence order.
2. Slice from each user entry to the next user entry.
3. Inspect only the first assistant entry in that slice.
4. Match its call IDs only inside that bounded slice, avoiding the repository's known non-global tool-call-ID problem.
5. Prefer bounded `ToolExecutionObserved` outcomes; use a matching `AgentMessage::Tool { is_error }` as the legacy fallback.
6. Add only succeeded names to the expected set.
7. Apply one deterministic skip reason when no eligible label remains: malformed, text-only, continuation-only, failed, cancelled, declined, obsolete-tool, or over-five-label.

Keep prompt text only in the private in-memory `EvalExample`; never derive `Debug`/`Serialize` for that type and never include it in errors or reports.

- [ ] **Step 5: Implement catalogue parsing, evaluation, fingerprints, and aggregate reporting**

Parse a top-level JSON array and convert each flat or nested provider schema through `AgentToolDefinition::from_provider_schema`. Reject duplicate names and an empty catalogue.

For each eligible example, render candidates with Task 1's shared renderer, call `retrieve_tools` once, collect top-one/three/five strict passes, elapsed microseconds, and misses by expected tool name. Compute:

```rust
fn decision(eligible: usize, top_five_passes: usize) -> NeedleEvalDecision {
    if eligible < 200 {
        NeedleEvalDecision::Inconclusive
    } else if top_five_passes.saturating_mul(100) >= eligible.saturating_mul(99) {
        NeedleEvalDecision::Pass
    } else {
        NeedleEvalDecision::Fail
    }
}
```

Hash `.cact` bytes and compact serialized catalogue bytes with the existing `sha2` dependency. Format only aggregate counts, percentages, latencies, tool-name misses, and lowercase SHA-256 fingerprints.

- [ ] **Step 6: Export the evaluator module behind the feature**

Add to `crates/threadlane-runtime/src/lib.rs`:

```rust
#[cfg(feature = "needle")]
pub mod needle_history_eval;
```

- [ ] **Step 7: Run evaluator and runtime tests**

```bash
cargo test -p threadlane-runtime --features needle needle_history_eval::tests
cargo test -p threadlane-runtime --features needle
```

Expected: all non-ignored tests pass.

- [ ] **Step 8: Commit the evaluator library**

```bash
git add crates/threadlane-runtime/src/needle_history_eval.rs crates/threadlane-runtime/src/lib.rs
git commit -m "feat(runtime): evaluate Needle against session history"
```

---

### Task 4: Add the developer evaluation command

**Files:**
- Create: `crates/threadlane-runtime/src/bin/needle_history_eval.rs`
- Modify: `crates/threadlane-runtime/Cargo.toml`

**Interfaces:**
- Consumes: `run_needle_history_eval` and `NeedleEvalConfig` from Task 3.
- Produces: `needle-history-eval --sessions <directory> --tools <provider-tools.json>`.
- Produces: exit `3` for argument/input/model errors; report decisions retain `0`/`1`/`2`.

- [ ] **Step 1: Declare the feature-gated binary and add parser tests**

In `Cargo.toml`:

```toml
[[bin]]
name = "needle-history-eval"
path = "src/bin/needle_history_eval.rs"
required-features = ["needle"]
```

In the binary, test an iterator-based parser so no process spawning is needed:

```rust
#[test]
fn parses_required_explicit_paths() {
    let config = parse_args([
        "--sessions",
        "/tmp/sessions",
        "--tools",
        "/tmp/tools.json",
    ])
    .unwrap();
    assert_eq!(config.sessions_dir, PathBuf::from("/tmp/sessions"));
    assert_eq!(config.tools_path, PathBuf::from("/tmp/tools.json"));
}
```

Also reject missing values, duplicate flags, and unknown arguments with a one-line usage string.

- [ ] **Step 2: Run the binary tests and verify they fail**

```bash
cargo test -p threadlane-runtime --features needle --bin needle-history-eval
```

Expected: compilation fails until `parse_args` and `main` exist.

- [ ] **Step 3: Implement the stdlib-only CLI**

`main` parses `std::env::args().skip(1)`, calls the library, prints the aggregate report, and exits with `report.decision.exit_code()`. On errors, print only the safe error plus usage and exit `3`. Do not echo raw argument values because the session path may identify a project.

- [ ] **Step 4: Run CLI tests and a synthetic local smoke command**

```bash
cargo test -p threadlane-runtime --features needle --bin needle-history-eval
cargo run -p threadlane-runtime --features needle --bin needle-history-eval -- --help
```

Expected: tests pass; help prints the one-line usage and exits `0`.

- [ ] **Step 5: Commit the command**

```bash
git add crates/threadlane-runtime/Cargo.toml crates/threadlane-runtime/src/bin/needle_history_eval.rs
git commit -m "feat(runtime): add Needle history evaluator command"
```

---

### Task 5: Activate and accurately surface Needle in GPUI

**Files:**
- Modify: `crates/threadlane-gpui/Cargo.toml`
- Modify: `crates/threadlane-gpui/src/services/settings.rs`
- Modify: `crates/threadlane-gpui/src/screens/settings/view.rs`

**Interfaces:**
- Consumes: `threadlane_runtime::local_tool_router::validate_needle_model()` from Task 1.
- Preserves: existing `AppState::set_needle_enabled` error propagation and persisted boolean format.

- [ ] **Step 1: Add a service-level validation test using an explicit missing weight path helper**

Keep environment mutation out of parallel tests. Add a path-taking runtime helper used by the cached environment accessor:

```rust
#[cfg(feature = "needle")]
#[test]
fn missing_model_is_rejected_before_enablement() {
    let missing = std::path::Path::new("/definitely/missing/needle2.cact");
    assert!(validate_needle_model_path(missing).is_err());
}
```

This test belongs in `local_tool_router.rs`; the GPUI service simply propagates the checked error before writing preferences.

- [ ] **Step 2: Run the validation test and verify it fails**

```bash
cargo test -p threadlane-runtime --features needle missing_model_is_rejected_before_enablement
```

Expected: compilation fails because `validate_needle_model_path` does not exist.

- [ ] **Step 3: Add the path helper and guard settings persistence**

Implement `validate_needle_model_path(&Path) -> Result<V2Engine, String>` under the feature, use it inside the cached engine accessor, and keep the public `validate_needle_model() -> Result<(), String>` interface.

At the start of `save_needle_enabled`:

```rust
if enabled {
    threadlane_runtime::local_tool_router::validate_needle_model()?;
}
```

This leaves the prior persisted value untouched when validation fails; `AppState::set_needle_enabled` already propagates the error to `capability_status`.

- [ ] **Step 4: Enable the runtime feature and correct settings copy**

Change the desktop dependency to:

```toml
threadlane-runtime = { version = "0.1.0", path = "../threadlane-runtime", features = ["needle"] }
```

Use “Local Needle Tool Routing” and “Shortlist the five most relevant tools locally before the first provider request.” Keep the existing switch, tag, and tooltip structure.

- [ ] **Step 5: Run focused validation**

```bash
cargo test -p threadlane-runtime --features needle missing_model_is_rejected_before_enablement
cargo check -p threadlane-gpui
```

Expected: both commands pass. Do not claim visual verification unless the app is run and observed.

- [ ] **Step 6: Commit GPUI activation**

```bash
git add crates/threadlane-gpui/Cargo.toml crates/threadlane-gpui/src/services/settings.rs crates/threadlane-gpui/src/screens/settings/view.rs crates/threadlane-runtime/src/local_tool_router.rs
git commit -m "feat(gpui): enable local Needle tool routing"
```

---

### Task 6: Document the workflow and run final verification

**Files:**
- Modify: `README.md`
- Modify: `AGENTS.md`

**Interfaces:**
- Documents: local-only weights and `THREADLANE_NEEDLE_WEIGHTS`.
- Documents: provider-tool JSON input and evaluator exit statuses.
- Records: first-attempt-only, top-five, fail-open repository convention.

- [ ] **Step 1: Add the runnable evaluator workflow to README**

Under “Development & Verification,” add:

````markdown
### Local Needle tool-routing evaluation

Needle weights are not distributed with Threadlane. Place `needle2.cact` at
`needle/needle2.cact` or set `THREADLANE_NEEDLE_WEIGHTS` to an explicit local
file. Evaluate the current provider-format tool catalogue against canonical
project sessions with:

```bash
cargo run -p threadlane-runtime --features needle --bin needle-history-eval -- \
  --sessions /path/to/project/.threadlane/sessions \
  --tools /path/to/provider-tools.json
```

The command is read-only and prints aggregates only. Exit codes are `0` pass,
`1` below 99% top-five recall, `2` fewer than 200 eligible turns, and `3`
invalid input or unavailable model.
````

- [ ] **Step 2: Record the durable convention in AGENTS.md**

Add this repository-specific rule near model/provider routing:

```markdown
- Needle local routing uses its contrastive retrieval head to shortlist at most five tools only on provider attempt one when the last model-visible message is a user message. Retries, continuation attempts, and every unavailable, busy, invalid, failed, or timed-out retrieval path receive the full configured catalogue. Historical evaluation is read-only and must never print prompts, arguments, results, session identifiers, or session paths.
```

- [ ] **Step 3: Run the complete required verification**

```bash
cargo test -p threadlane-runtime --features needle
cargo check -p threadlane-gpui
git diff --check
```

Expected: all non-ignored tests pass, GPUI checks successfully, and whitespace validation prints nothing.

- [ ] **Step 4: Run the real-model retrieval test when local weights exist**

```bash
cargo test -p threadlane-runtime --features needle needle_v2_retrieves_weather_in_top_five -- --ignored --nocapture
```

Expected: the synthetic weather tool appears within Needle's top five. If weights are absent, report the skipped manual verification without changing the test.

- [ ] **Step 5: Inspect scope before the final commit**

```bash
git status --short
git diff --stat
```

Expected: only files named in this plan plus any pre-existing unrelated user changes appear. Do not stage `crates/threadlane-gpui/src/screens/right_panel/view.rs`.

- [ ] **Step 6: Commit documentation**

```bash
git add README.md AGENTS.md
git commit -m "docs: explain Needle routing evaluation"
```

- [ ] **Step 7: Record the aggregate local evaluation outside Git**

Run the evaluator with the user's explicit session directory and tool catalogue. Report only the aggregate decision and metrics in the task handoff; do not save prompts, arguments, results, or a report file in the repository.

---

## Completion Criteria

- Needle retrieval returns at most five ranked tool definitions on eligible first attempts.
- Every continuation and failure path sends the complete catalogue.
- The evaluator reads canonical JSONL without writes and emits aggregate-safe output.
- `200` eligible turns with `198` or more strict top-five passes is `pass`; fewer than `200` is `inconclusive`.
- The GPUI build compiles Needle support and rejects enabling an unavailable model.
- The ignored real-model check passes when the local `.cact` file is present.
- Runtime tests, GPUI check, and whitespace validation pass.
- Model distribution, LoRA, online learning, and new dependencies remain absent.
